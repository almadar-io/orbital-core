//! What an effect puts on the bus when its `emit.success` fires.
//!
//! **The payload invariant:** an event's declared payload type IS its access
//! path. Declare `Survey` and the payload is the row, read as `?name`; declare
//! `{ row : Survey }` and it is read as `?row.name`. Wrapping that is not in
//! the type is a lie, and a field in the type that is not in the payload reads
//! `undefined`. Reading FEWER fields than are emitted is fine, so the relation
//! is `declared ⊆ produced`.
//!
//! This module is the ONE place that answers "what does operator X emit?" for
//! both compiler layers. It lives in `orbital-core` because
//! `orbital-lolo` (L1) and `orbital-compiler` (L2) are siblings — L1 dev-depends
//! on L2, so it cannot reach the baked operator registry, and CLAUDE.md's rule
//! is that a check both layers need lives in the crate they share.
//!
//! The two layers see different amounts of the program, and the split is
//! deliberate:
//!
//! - **L1** knows one file. It can decide a call site whose entity is a literal
//!   in the same file, and it catches the common authoring mistake at parse
//!   time.
//! - **L2** knows the whole program post-inline, including `@rebindable` traits
//!   whose entity is only fixed once a consumer binds it. Those are inherently
//!   L2 and L1 stays silent on them.
//!
//! The contracts below are LANGUAGE invariants, not registry trivia, but the
//! registry (`canonical-operators.json`) states them too. To keep the two from
//! drifting, `orbital-compiler` carries a test that asserts this table agrees
//! with the baked registry — drift fails a build rather than waiting to be
//! noticed in a corpus sweep.

/// The shape an operator's success emit carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProducedPayload {
    /// The persisted row itself, FLAT — its own fields at the payload root.
    /// `persist create` / `persist update`: both executors emit the row with
    /// no wrapper (`EffectExecutor.ts`'s `successPayload = persisted ?? data`;
    /// `orbital-core`'s `do_persist` → `executed_with_value`).
    EntityRow,
    /// A fixed set of field names, e.g. `persist delete`'s `{ id, deleted }`
    /// or `fetch`'s `{ data, totalCount }` wrapper.
    Fields(&'static [&'static str]),
    /// Not decidable here — no contract, an opaque `any`, or a call site this
    /// layer cannot narrow. Callers MUST treat this as "do not check" rather
    /// than as "produces nothing".
    Opaque,
}

/// `persist delete` hands back the deleted row's id plus a tombstone flag —
/// never the row.
pub const PERSIST_DELETE_FIELDS: &[&str] = &["id", "deleted"];

/// `fetch` / `ref` wrap their rows: authors read `?data`, and `totalCount` is
/// the pre-pagination count so a paginating consumer needs no second call.
pub const FETCH_WRAPPER_FIELDS: &[&str] = &["data", "totalCount"];

/// `persist batch` hands back a summary of the attempted operations.
pub const PERSIST_BATCH_FIELDS: &[&str] = &["operations", "completedCount", "totalCount"];

/// What `operator` emits on success at a call site.
///
/// `persist_action` is the literal action keyword when the call site carries
/// one (`["persist", "create", …]`). The legacy 3-element form
/// (`["persist", "Entity", data]`) has no action keyword; pass `None` and this
/// returns [`ProducedPayload::Opaque`] rather than guessing "create" — a wrong
/// guess would report against the wrong contract.
pub fn success_payload(operator: &str, persist_action: Option<&str>) -> ProducedPayload {
    match operator {
        "persist" => match persist_action {
            Some("create") | Some("update") => ProducedPayload::EntityRow,
            Some("delete") => ProducedPayload::Fields(PERSIST_DELETE_FIELDS),
            Some("batch") => ProducedPayload::Fields(PERSIST_BATCH_FIELDS),
            _ => ProducedPayload::Opaque,
        },
        // `ref` emits the same wrapper as `fetch` but fires it on `on_change`;
        // see `success_emit_key`.
        "fetch" | "ref" => ProducedPayload::Fields(FETCH_WRAPPER_FIELDS),
        // `call-service` and `set` declare `any`; nothing to compare against.
        _ => ProducedPayload::Opaque,
    }
}

/// The bus key an operator fires its success payload on.
///
/// `ref` uses `on_change` in both executors, so reading `emit.success` for it
/// inspects a key the runtime never fires.
pub fn success_emit_key(operator: &str) -> &'static str {
    match operator {
        "ref" => "on_change",
        _ => "success",
    }
}

/// The declared field names that the produced payload does NOT carry — i.e.
/// every field whose read would be `undefined` at runtime.
///
/// This is the whole rule, and it is width subtyping on purpose: declaring
/// FEWER fields than are emitted is legal (you only read what you declared),
/// so only the declared side is walked. Equality would reject the many corpus
/// fetch events that legitimately omit `totalCount`.
///
/// `entity_field_names` supplies the row's own fields for
/// [`ProducedPayload::EntityRow`]; pass an empty slice when the entity cannot
/// be resolved, which yields no findings rather than false ones.
pub fn undeclared_reads<'a>(
    declared_field_names: &[&'a str],
    produced: &ProducedPayload,
    entity_field_names: &[&str],
) -> Vec<&'a str> {
    let produced_names: &[&str] = match produced {
        ProducedPayload::Opaque => return Vec::new(),
        ProducedPayload::Fields(f) => f,
        ProducedPayload::EntityRow => {
            if entity_field_names.is_empty() {
                return Vec::new();
            }
            entity_field_names
        }
    };
    declared_field_names
        .iter()
        .copied()
        .filter(|d| !produced_names.contains(d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_create_and_update_emit_the_row_flat() {
        assert_eq!(
            success_payload("persist", Some("create")),
            ProducedPayload::EntityRow
        );
        assert_eq!(
            success_payload("persist", Some("update")),
            ProducedPayload::EntityRow
        );
    }

    #[test]
    fn persist_delete_emits_a_tombstone_not_the_row() {
        assert_eq!(
            success_payload("persist", Some("delete")),
            ProducedPayload::Fields(PERSIST_DELETE_FIELDS)
        );
    }

    /// The legacy 3-element persist form carries no action keyword. Guessing
    /// "create" there would compare against the wrong contract.
    #[test]
    fn an_actionless_persist_is_not_guessed_at() {
        assert_eq!(success_payload("persist", None), ProducedPayload::Opaque);
    }

    #[test]
    fn fetch_and_ref_emit_the_data_wrapper_and_ref_uses_on_change() {
        assert_eq!(
            success_payload("fetch", None),
            ProducedPayload::Fields(FETCH_WRAPPER_FIELDS)
        );
        assert_eq!(success_emit_key("fetch"), "success");
        assert_eq!(success_emit_key("ref"), "on_change");
    }

    /// The defect that started this: `persist create` emits the row flat, so a
    /// declared `row` wrapper names a field nothing emits.
    #[test]
    fn a_row_wrapper_on_a_created_entity_is_reported() {
        let found = undeclared_reads(
            &["row"],
            &ProducedPayload::EntityRow,
            &["id", "title", "content"],
        );
        assert_eq!(found, vec!["row"]);
    }

    /// Omission is legal — the rule is `declared ⊆ produced`.
    #[test]
    fn declaring_a_subset_is_legal() {
        assert!(undeclared_reads(
            &["data"],
            &ProducedPayload::Fields(FETCH_WRAPPER_FIELDS),
            &[]
        )
        .is_empty());
        assert!(
            undeclared_reads(&["id"], &ProducedPayload::EntityRow, &["id", "title"]).is_empty()
        );
    }

    /// An unresolvable entity must produce NO finding rather than claim every
    /// declared field is missing.
    #[test]
    fn an_unresolvable_entity_yields_no_findings() {
        assert!(undeclared_reads(&["anything"], &ProducedPayload::EntityRow, &[]).is_empty());
    }

    #[test]
    fn opaque_operators_are_never_checked() {
        assert_eq!(
            success_payload("call-service", None),
            ProducedPayload::Opaque
        );
        assert!(undeclared_reads(&["whatever"], &ProducedPayload::Opaque, &[]).is_empty());
    }
}
