//! Identity model (Almadar Rabit V4, Phase 1 — types only).
//!
//! Rust mirror of `@almadar/core`'s `src/types/identity.ts`: kind-prefixed
//! node ids plus the workspace name ledger (`id → {bakedName, curName,
//! renames, owner}`). Ids are the durable edge keys of the identity-keyed
//! schema graph; names are display labels, one ledger row per rename. The
//! wire form is identical to the TS side (camelCase keys, ids as plain
//! strings). Minting lives in the L1 lowerer (Phase 2) — this module only
//! classifies and round-trips ids; it never creates them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Node kind ↔ id-prefix table. Must stay in lockstep with `ID_PREFIXES`
/// in `@almadar/core` `src/types/identity.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdKind {
    Orbital,
    Entity,
    Trait,
    Event,
    Page,
    Service,
    Theme,
    Palette,
}

impl IdKind {
    pub const ALL: [IdKind; 8] = [
        IdKind::Orbital,
        IdKind::Entity,
        IdKind::Trait,
        IdKind::Event,
        IdKind::Page,
        IdKind::Service,
        IdKind::Theme,
        IdKind::Palette,
    ];

    pub fn prefix(self) -> &'static str {
        match self {
            IdKind::Orbital => "orb_",
            IdKind::Entity => "ent_",
            IdKind::Trait => "trt_",
            IdKind::Event => "evt_",
            IdKind::Page => "pag_",
            IdKind::Service => "svc_",
            IdKind::Theme => "thm_",
            IdKind::Palette => "pal_",
        }
    }
}

/// Classify an id string by its kind prefix. `None` for unknown prefixes or
/// a bare prefix with no suffix.
pub fn id_kind_of(value: &str) -> Option<IdKind> {
    IdKind::ALL
        .into_iter()
        .find(|kind| value.len() > kind.prefix().len() && value.starts_with(kind.prefix()))
}

/// Whether `value` is a well-formed id of `kind`.
pub fn is_id_of_kind(value: &str, kind: IdKind) -> bool {
    id_kind_of(value) == Some(kind)
}

/// Ledger row owner: which package (or the workspace) minted the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerOwner {
    Std,
    Io,
    Workspace,
}

/// One ledger row: the name history for a single node id.
///
/// For kind `event` the row is minted per-(trait, declared event) per the
/// Phase-0 freeze — the owning trait id is recorded in `parent`; a call-site
/// event rename is a one-row edit on this same id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: String,
    pub kind: IdKind,
    #[serde(rename = "bakedName")]
    pub baked_name: String,
    #[serde(rename = "curName")]
    pub cur_name: String,
    pub renames: Vec<RenameRecord>,
    pub owner: LedgerOwner,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameRecord {
    pub from: String,
    pub to: String,
    pub at: String,
}

/// The workspace name ledger. `entries` is keyed by id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityLedger {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Ordered, so a serialized ledger is byte-identical across runs.
    pub entries: BTreeMap<String, LedgerEntry>,
}

impl IdentityLedger {
    /// Resolve a name to its id via exact `curName` match within `kind`.
    pub fn resolve_name(&self, kind: IdKind, name: &str) -> Option<&str> {
        self.entries
            .values()
            .find(|e| e.kind == kind && e.cur_name == name)
            .map(|e| e.id.as_str())
    }

    /// Current display name for an id.
    pub fn cur_name(&self, id: &str) -> Option<&str> {
        self.entries.get(id).map(|e| e.cur_name.as_str())
    }

    /// Rename the node behind `id`, appending to its rename history.
    /// No-op when the id is unknown.
    pub fn rename(&mut self, id: &str, to: &str, at: &str) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.renames.push(RenameRecord {
                from: entry.cur_name.clone(),
                to: to.to_string(),
                at: at.to_string(),
            });
            entry.cur_name = to.to_string();
        }
    }
}

// =============================================================================
// Minting (Phase 2)
// =============================================================================
//
// Ids are `<prefix><26-char Crockford ULID>`: a 48-bit millisecond timestamp
// (10 chars) + 80 bits of randomness (16 chars). Timestamp comes from the
// wasm-safe `crate::time_util` clock; the 80 bits of entropy are caller-
// supplied so tests are deterministic. The default `std_entropy` source is
// NON-CRYPTOGRAPHIC (a hashed clock + a process-global counter) — ids need
// uniqueness, not secrecy, and we deliberately avoid a `rand`/`getrandom`
// dependency (which does not build cleanly on `wasm32-unknown-unknown`).

/// Crockford base32 alphabet (excludes I, L, O, U). Same ordering ULID uses.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Emit the low `n_chars * 5` bits of `value` as base32, most-significant char
/// first, appending to `out`.
fn push_base32(out: &mut String, mut value: u128, n_chars: usize) {
    let mut buf = vec![0u8; n_chars];
    for slot in buf.iter_mut().rev() {
        *slot = CROCKFORD[(value & 0x1f) as usize];
        value >>= 5;
    }
    out.push_str(std::str::from_utf8(&buf).expect("crockford alphabet is ascii"));
}

/// SplitMix64 — a small, well-distributed non-cryptographic mixer.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// FNV-1a 64-bit over `data` starting from `offset`.
fn fnv1a64(data: &[u8], offset: u64) -> u64 {
    let mut h = offset;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Mint a fresh kind-prefixed ULID-style id: `<prefix><10-char time><16-char
/// random>`. `entropy(10)` supplies the 80 random bits; the timestamp comes
/// from `crate::time_util`. Caller-supplied entropy keeps tests deterministic.
pub fn mint_id(kind: IdKind, entropy: &mut impl FnMut(usize) -> Vec<u8>) -> String {
    let ts = (crate::time_util::now_millis().max(0) as u128) & ((1u128 << 48) - 1);
    let bytes = entropy(10);
    let mut rand: u128 = 0;
    for i in 0..10 {
        rand = (rand << 8) | (*bytes.get(i).unwrap_or(&0) as u128);
    }
    let mut s = String::with_capacity(kind.prefix().len() + 26);
    s.push_str(kind.prefix());
    push_base32(&mut s, ts, 10);
    push_base32(&mut s, rand & ((1u128 << 80) - 1), 16);
    s
}

/// This process's entropy seed. Every process gets its own, so two processes
/// minting in the same millisecond never share an id sequence (G-ORB-006).
pub fn process_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SEED.get_or_init(|| {
        // `RandomState`'s keys come from the OS's randomness, fresh per process.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        #[cfg(not(target_arch = "wasm32"))]
        h.write_u32(std::process::id());
        h.write_i64(crate::time_util::now_millis());
        h.finish()
    })
}

/// The default, NON-CRYPTOGRAPHIC entropy source: this process's seed, the clock
/// and a monotonically-advancing counter, diffused through SplitMix64. The counter
/// keeps calls in one process distinct; the seed keeps processes apart. wasm-safe
/// (the clock via `crate::time_util`, never `getrandom`).
pub fn std_entropy() -> impl FnMut(usize) -> Vec<u8> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    move |n| {
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        seeded_entropy(
            process_seed(),
            crate::time_util::now_millis() as u64,
            counter,
            n,
        )
    }
}

/// `n` bytes from a process seed, a clock reading and a per-process counter.
fn seeded_entropy(seed: u64, clock_ms: u64, counter: u64, n: usize) -> Vec<u8> {
    let base = splitmix64(seed ^ clock_ms) ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(n);
    let mut ctr = 0u64;
    while out.len() < n {
        out.extend_from_slice(&splitmix64(base.wrapping_add(ctr)).to_le_bytes());
        ctr += 1;
    }
    out.truncate(n);
    out
}

/// Deterministically derive a child id rooted in `parent_id` — used for
/// synthetic nodes produced by hoisting (render-embedded children). Same
/// `(parent_id, discriminator)` always yields the same id (no entropy, no
/// clock), so re-lowering never churns. The child keeps `parent_id`'s kind
/// prefix; the 26-char body is a hash of `(parent_id, discriminator)`, so the
/// embed edge is recoverable from the id itself.
pub fn derive_id(parent_id: &str, discriminator: &str) -> String {
    let kind = id_kind_of(parent_id).unwrap_or(IdKind::Trait);
    let mut data = Vec::with_capacity(parent_id.len() + 1 + discriminator.len());
    data.extend_from_slice(parent_id.as_bytes());
    data.push(0x1f);
    data.extend_from_slice(discriminator.as_bytes());
    hashed_id(kind, &data)
}

/// A content-derived id for a declaration node: the same `(scope, kind, name,
/// parent, ordinal)` always yields the same id, so lowering a source with no
/// prior ledger is reproducible with nothing stored. `scope` is the program's
/// own name (two behaviors composed into one program never collide); `ordinal`
/// tells same-key siblings apart in encounter order.
pub fn derive_node_id(
    kind: IdKind,
    scope: &str,
    baked: &str,
    parent: Option<&str>,
    ordinal: usize,
) -> String {
    let mut data = Vec::new();
    for part in [kind.prefix(), scope, baked, parent.unwrap_or("")] {
        data.extend_from_slice(part.as_bytes());
        data.push(0x1f);
    }
    data.extend_from_slice(&(ordinal as u64).to_le_bytes());
    hashed_id(kind, &data)
}

fn hashed_id(kind: IdKind, data: &[u8]) -> String {
    let h1 = splitmix64(fnv1a64(data, 0xcbf2_9ce4_8422_2325));
    let h2 = splitmix64(fnv1a64(data, 0x0000_0100_0000_01B3 ^ 0xDEAD_BEEF));
    let time_bits = (((h1 as u128) << 64) | h2 as u128) & ((1u128 << 50) - 1);
    let rand_bits = (((h2 as u128) << 64) | h1 as u128) & ((1u128 << 80) - 1);
    let mut s = String::with_capacity(kind.prefix().len() + 26);
    s.push_str(kind.prefix());
    push_base32(&mut s, time_bits, 10);
    push_base32(&mut s, rand_bits, 16);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq_entropy() -> impl FnMut(usize) -> Vec<u8> {
        let mut ctr: u64 = 0;
        move |n| {
            ctr += 1;
            let mut out = Vec::with_capacity(n);
            let mut i = 0u64;
            while out.len() < n {
                out.extend_from_slice(&splitmix64(ctr.wrapping_add(i)).to_le_bytes());
                i += 1;
            }
            out.truncate(n);
            out
        }
    }

    #[test]
    fn mint_id_shape_and_kind() {
        let mut e = seq_entropy();
        let id = mint_id(IdKind::Trait, &mut e);
        assert!(id.starts_with("trt_"), "prefix: {id}");
        assert_eq!(id.len(), "trt_".len() + 26, "26-char body: {id}");
        assert_eq!(id_kind_of(&id), Some(IdKind::Trait));
        // Every body char is in the Crockford alphabet.
        assert!(id["trt_".len()..].bytes().all(|b| CROCKFORD.contains(&b)));
    }

    #[test]
    fn mint_id_unique_across_calls_with_stateful_entropy() {
        let mut e = std_entropy();
        let a = mint_id(IdKind::Entity, &mut e);
        let b = mint_id(IdKind::Entity, &mut e);
        assert_ne!(a, b, "distinct entropy must yield distinct ids");
    }

    const SEED_LINE: &str = "PROCESS_SEED=";

    /// Run only as a child of `every_process_gets_its_own_seed`.
    #[test]
    #[ignore]
    fn print_process_seed() {
        println!("{SEED_LINE}{}", process_seed());
    }

    #[test]
    fn every_process_gets_its_own_seed() {
        let exe = std::env::current_exe().expect("test binary path");
        let children: Vec<_> = (0..8)
            .map(|_| {
                std::process::Command::new(&exe)
                    .args([
                        "--exact",
                        "identity::tests::print_process_seed",
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .output()
                    .expect("spawn the test binary")
            })
            .collect();
        let seeds: std::collections::BTreeSet<String> = children
            .iter()
            .map(|out| {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout
                    .lines()
                    .find_map(|l| {
                        l.split_once(SEED_LINE)
                            .map(|(_, seed)| seed.trim().to_string())
                    })
                    .unwrap_or_else(|| panic!("no seed line in child output: {stdout}"))
            })
            .collect();
        assert_eq!(
            seeds.len(),
            children.len(),
            "processes shared a seed: {seeds:?}"
        );
    }

    #[test]
    fn distinct_seeds_at_the_same_clock_mint_distinct_ids() {
        let a = seeded_entropy(1, 1_790_000_000_000, 0, 10);
        let b = seeded_entropy(2, 1_790_000_000_000, 0, 10);
        assert_ne!(a, b);
    }

    #[test]
    fn control_the_same_seed_clock_and_counter_are_deterministic() {
        assert_eq!(
            seeded_entropy(7, 1_790_000_000_000, 3, 10),
            seeded_entropy(7, 1_790_000_000_000, 3, 10)
        );
        assert_ne!(
            seeded_entropy(7, 1_790_000_000_000, 3, 10),
            seeded_entropy(7, 1_790_000_000_000, 4, 10),
            "the in-process counter still separates consecutive mints"
        );
        assert_eq!(seeded_entropy(7, 0, 0, 13).len(), 13, "any length");
    }

    #[test]
    fn derive_id_is_deterministic_and_parent_rooted() {
        let parent = "trt_01HZX5AAAAAAAAAAAAAAAAAA";
        let a = derive_id(parent, "InlineButtonRender1");
        let b = derive_id(parent, "InlineButtonRender1");
        assert_eq!(a, b, "same parent+discriminator ⇒ same id");
        assert!(a.starts_with("trt_"), "keeps parent kind prefix: {a}");
        assert_eq!(a.len(), "trt_".len() + 26);
        let c = derive_id(parent, "InlineButtonRender2");
        assert_ne!(a, c, "distinct discriminators ⇒ distinct ids");
        let d = derive_id("trt_01HZX5BBBBBBBBBBBBBBBBBB", "InlineButtonRender1");
        assert_ne!(a, d, "distinct parents ⇒ distinct ids");
    }

    #[test]
    fn classifies_ids_by_prefix() {
        assert_eq!(id_kind_of("orb_01HZX5"), Some(IdKind::Orbital));
        assert_eq!(id_kind_of("ent_01HZX5"), Some(IdKind::Entity));
        assert_eq!(id_kind_of("trt_01HZX5"), Some(IdKind::Trait));
        assert_eq!(id_kind_of("evt_01HZX5"), Some(IdKind::Event));
        assert_eq!(id_kind_of("pag_01HZX5"), Some(IdKind::Page));
        assert_eq!(id_kind_of("svc_01HZX5"), Some(IdKind::Service));
        assert_eq!(id_kind_of("thm_01HZX5"), Some(IdKind::Theme));
        assert_eq!(id_kind_of("pal_01HZX5"), Some(IdKind::Palette));
        assert_eq!(id_kind_of("orb_"), None);
        assert_eq!(id_kind_of("Task"), None);
        assert!(is_id_of_kind("trt_x", IdKind::Trait));
        assert!(!is_id_of_kind("trt_x", IdKind::Entity));
    }

    /// Wire-parity fixture: the exact JSON shape `@almadar/core`'s
    /// `IdentityLedger` serializes (camelCase keys, lowercase kind/owner).
    #[test]
    fn ledger_round_trips_ts_wire_shape() {
        let json = r#"{
            "schemaVersion": 1,
            "entries": {
                "ent_01HZX5AAAAAAAAAAAAAAAAAA": {
                    "id": "ent_01HZX5AAAAAAAAAAAAAAAAAA",
                    "kind": "entity",
                    "bakedName": "Course",
                    "curName": "PotteryClass",
                    "renames": [{"from": "Course", "to": "PotteryClass", "at": "2026-07-14T00:00:00Z"}],
                    "owner": "workspace"
                },
                "evt_01HZX5BBBBBBBBBBBBBBBBBB": {
                    "id": "evt_01HZX5BBBBBBBBBBBBBBBBBB",
                    "kind": "event",
                    "bakedName": "OPEN",
                    "curName": "ADD_ITEM",
                    "renames": [],
                    "owner": "std",
                    "parent": "trt_01HZX5CCCCCCCCCCCCCCCCCC"
                }
            }
        }"#;
        let ledger: IdentityLedger = serde_json::from_str(json).unwrap();
        assert_eq!(ledger.schema_version, 1);
        assert_eq!(
            ledger.resolve_name(IdKind::Entity, "PotteryClass"),
            Some("ent_01HZX5AAAAAAAAAAAAAAAAAA")
        );
        assert_eq!(ledger.resolve_name(IdKind::Entity, "Course"), None);
        assert_eq!(
            ledger.cur_name("evt_01HZX5BBBBBBBBBBBBBBBBBB"),
            Some("ADD_ITEM")
        );
        let back = serde_json::to_value(&ledger).unwrap();
        assert_eq!(back["schemaVersion"], 1);
        assert_eq!(
            back["entries"]["ent_01HZX5AAAAAAAAAAAAAAAAAA"]["curName"],
            "PotteryClass"
        );
        assert_eq!(
            back["entries"]["evt_01HZX5BBBBBBBBBBBBBBBBBB"]["parent"],
            "trt_01HZX5CCCCCCCCCCCCCCCCCC"
        );
        assert_eq!(
            back["entries"]["ent_01HZX5AAAAAAAAAAAAAAAAAA"]["kind"],
            "entity"
        );
        assert_eq!(
            back["entries"]["ent_01HZX5AAAAAAAAAAAAAAAAAA"]["owner"],
            "workspace"
        );
    }

    #[test]
    fn rename_appends_history_and_updates_cur_name() {
        let mut ledger = IdentityLedger {
            schema_version: 1,
            entries: BTreeMap::from([(
                "trt_x1".to_string(),
                LedgerEntry {
                    id: "trt_x1".to_string(),
                    kind: IdKind::Trait,
                    baked_name: "LessonPlayer".to_string(),
                    cur_name: "LessonPlayer".to_string(),
                    renames: vec![],
                    owner: LedgerOwner::Std,
                    parent: None,
                },
            )]),
        };
        ledger.rename("trt_x1", "CoursePlayer", "2026-07-14T00:00:00Z");
        assert_eq!(ledger.cur_name("trt_x1"), Some("CoursePlayer"));
        assert_eq!(
            ledger.resolve_name(IdKind::Trait, "CoursePlayer"),
            Some("trt_x1")
        );
        assert_eq!(ledger.resolve_name(IdKind::Trait, "LessonPlayer"), None);
        let entry = &ledger.entries["trt_x1"];
        assert_eq!(entry.renames.len(), 1);
        assert_eq!(entry.renames[0].from, "LessonPlayer");
        assert_eq!(entry.baked_name, "LessonPlayer");
        ledger.rename("trt_missing", "X", "now");
    }
}
