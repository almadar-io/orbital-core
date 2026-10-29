#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use crate::schema::types::EntityDefinition;
use crate::schema::types::EntityRef;
use crate::schema::sexpr::SExpression;
use crate::schema::types::OrbitalSchema;
use crate::value::Value;
use std::collections::HashMap;


/// Value for a declared `@mock "..."` field — the declared string used
/// LITERALLY, never interpreted through any domain-name lookup. Comma-
/// separated is an explicit candidate list the author supplies real values
/// for, rotated by row index (`@mock "Acme Corp, Globex Inc, Wayne
/// Enterprises"` -> row 1 gets "Acme Corp", row 2 "Globex Inc", ...). No
/// comma is a single literal value, index-suffixed so rows aren't
/// byte-identical (`@mock "person-name"` -> "person-name-1",
/// "person-name-2", ...). Mirrors `@almadar/core/mock`'s twin exactly —
/// same split/rotate/suffix rule, no per-domain code on either side.
/// A declared `@mock`'s comma-separated candidates, trimmed, empties dropped.
/// Twin: `mockCandidates` in @almadar/core `mock/sampleValue.ts`.
pub fn mock_candidates(declared: &str) -> Vec<&str> {
    declared
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// The schema's `[identity]` entity definition, if it declares one.
///
/// At most one per composed program (`ORB_S_IDENTITY_NOT_UNIQUE`); same-name
/// duplicates across orbitals are already rejected upstream by
/// `validate_entity_uniqueness`, so the first match is the only match.
pub fn identity_entity_def(schema: &OrbitalSchema) -> Option<&EntityDefinition> {
    schema.orbitals.iter().find_map(|orbital| {
        std::iter::once(&orbital.entity)
            .chain(orbital.auxiliary_entities.iter().flatten())
            .find_map(|er| match er {
                EntityRef::Inline(def) if def.identity => Some(def),
                _ => None,
            })
    })
}

/// Every inline entity definition in the schema: each orbital's primary entity
/// plus its auxiliaries. Shared by every walk over "every entity the schema
/// declares" ([`owner_fields_from_schema`], self-identity, the policy-derived
/// arm, and `entity_access::role_satisfying_policy`'s entity lookup) so they
/// see identically-scoped entity sets. `pub(crate)` for the sibling module.
pub fn inline_entity_defs(schema: &OrbitalSchema) -> Vec<&EntityDefinition> {
    schema
        .orbitals
        .iter()
        .flat_map(|orbital| {
            std::iter::once(&orbital.entity).chain(orbital.auxiliary_entities.iter().flatten())
        })
        .filter_map(|er| match er {
            EntityRef::Inline(def) => Some(def),
            _ => None,
        })
        .collect()
}

/// The name of the schema's `[identity]` entity, if it declares one.
pub fn identity_entity_name(schema: &OrbitalSchema) -> Option<&str> {
    identity_entity_def(schema).map(|def| def.name.as_str())
}

/// Owner columns DERIVED from the schema: every relation field pointing at the
/// `[identity]` entity. Declared in the source (`patientId : Person`), never
/// inferred from a field name — a name-matching guess would silently scope the
/// wrong column. `ALMADAR_PERSONA_OWNS` stays as an explicit override/addition
/// for schemas that have not been migrated to identity-typed columns yet.
/// The field name `x` reads off `binding` — either the dotted-path form
/// (`@entity.id`) or the lowered `.lolo` call form (`(object/get @entity id)` →
/// `["object/get", "@entity", "id"]`, the shape `std-time-tracking`'s `Employee
/// [identity]` actually compiles to; verified against `packages/almadar-behaviors/
/// behaviors/registry/app/organisms/std-time-tracking.orb`). `None` when `x` reads
/// a different binding, a different shape entirely, or isn't a field read at all.
/// JS twin: `fieldRefName` in `identityOwners.ts`.
pub fn field_ref_name(x: &SExpression, binding: &str) -> Option<String> {
    if let SExpression::String(s) = x {
        return s.strip_prefix(&format!("{binding}.")).map(str::to_string);
    }
    let SExpression::Array(items) = x else {
        return None;
    };
    match items.as_slice() {
        [op, b, SExpression::String(f)]
            if matches!(op, SExpression::String(s) if s == "object/get")
                && matches!(b, SExpression::String(s) if s == binding) =>
        {
            Some(f.clone().into_inner())
        }
        _ => None,
    }
}

/// `true` when `x` reads field `field` off binding `binding` (either spelling —
/// see [`field_ref_name`]). JS twin: `isFieldRef` in `identityOwners.ts`.
pub fn is_field_ref(x: &SExpression, binding: &str, field: &str) -> bool {
    field_ref_name(x, binding).as_deref() == Some(field)
}

/// Equality operator spellings a policy comparison may carry: `=` is what
/// `.lolo`-authored policies lower to; `==` is what `OirBinaryOp::Eq::symbol()`
/// spells when a codegen policy is bridged back through `OirExpr::to_sexpression`
/// (`access_policies.rs::owner_fields`) — the two derivations converge on this
/// one reader, so both spellings must resolve identically here.
pub const POLICY_EQ_OPS: [&str; 2] = ["=", "=="];

/// `true` for `@user.id` — either spelling (see [`field_ref_name`]).
pub fn is_viewer_id(x: &SExpression) -> bool {
    is_field_ref(x, "@user", "id")
}

/// Every column `expr` compares to `@user.id` via direct equality — the general
/// form of "self-identity": `@entity.<field> == @user.id` (either operand order,
/// either spelling — see [`field_ref_name`]) names `field` as an owner column,
/// whether `field` is the row's own `id` (self-identity — see
/// [`policies_self_identify`]'s doc) or any other declared field (the shape
/// `ORB_S_OWNER_FIELD_NOT_IDENTITY_TYPED` flags as an authoring error when it is
/// neither). Recurses through `and`/`or`/`not`/any other combinator, mirroring
/// the old `has_self_identity_equality` walk. JS twin: `hasSelfIdentityEquality`
/// generalized the same way in `identityOwners.ts`.
pub fn owner_columns_from_policy(expr: &SExpression) -> Vec<String> {
    let mut out = Vec::new();
    collect_owner_columns_from_policy(expr, &mut out);
    out
}

pub fn collect_owner_columns_from_policy(expr: &SExpression, out: &mut Vec<String>) {
    let SExpression::Array(items) = expr else {
        return;
    };
    if let [op, a, b] = items.as_slice() {
        if matches!(op, SExpression::String(s) if POLICY_EQ_OPS.contains(&s.as_str())) {
            let field = if is_viewer_id(b) {
                field_ref_name(a, "@entity")
            } else if is_viewer_id(a) {
                field_ref_name(b, "@entity")
            } else {
                None
            };
            if let Some(f) = field {
                if !out.contains(&f) {
                    out.push(f);
                }
            }
        }
    }
    for item in items {
        collect_owner_columns_from_policy(item, out);
    }
}

/// `true` when `def`'s own declared access directives self-identify the row —
/// the `[identity]` entity's OWN `id` is compared directly to `@user.id` (see
/// [`owner_columns_from_policy`]).
pub fn policies_self_identify(def: &EntityDefinition) -> bool {
    [
        &def.read_policy,
        &def.create_policy,
        &def.update_policy,
        &def.delete_policy,
    ]
    .into_iter()
    .flatten()
    .any(|expr| owner_columns_from_policy(expr).iter().any(|f| f == "id"))
}

/// Declared `(entity_key, owner_column)` pairs — every NON-INTRINSIC field typed
/// as a relation to the `[identity]` entity, PLUS `(identity_key, "id")` when the
/// `[identity]` entity's own declared access policy compares its row directly to
/// the viewer (self-identity), PLUS any other column an entity's own declared
/// `@read`/`@update`/`@delete` compares to `@user.id` (see
/// [`owner_columns_from_policy`] — the reader `access_policies.rs::owner_fields`
/// shares for the codegen side of this same derivation). Public so the
/// verify harness can stamp the SAME columns onto its synthetic row that real
/// seeding stamps onto sample rows; a walker row without them can never satisfy
/// an ownership predicate (`V-L1-WALKER-SYNTHETIC-ROW-HAS-NO-OWNER-COLUMN`).
///
/// An `intrinsic: true` relation field is excluded even when it targets the
/// identity entity: intrinsic marks framework plumbing the owning trait computes
/// itself (`FieldDefinition.intrinsic` doc, `schema/types.rs`), never domain
/// ownership — a generic atom's self-relation (`ModalRecord.seedRow :
/// ModalRecord`, record-detail/modal edit-seed plumbing) gets its relation target
/// rewritten onto whatever entity the trait binds, so a self-identity entity like
/// `std-time-tracking`'s `Employee [identity]` ends up with an `Employee.seedRow
/// : Employee` self-relation that is structurally indistinguishable from a real
/// owner column by type/target alone. Stamping it with the viewer id (as an
/// owner column would be) creates a row that references itself, which trips the
/// entity's own `onDelete: restrict`. JS twin: `ownerFieldsFromSchema` in
/// `identityOwners.ts`.
pub fn owner_fields_from_schema(schema: &OrbitalSchema) -> Vec<(String, String)> {
    let Some(identity) = identity_entity_name(schema) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let defs = inline_entity_defs(schema);
    let mut declared_by_collection: HashMap<String, Vec<String>> = HashMap::new();
    for def in &defs {
        for field in &def.fields {
            let Some(name) = field.name.as_ref() else {
                continue;
            };
            let points_at_identity = !field.intrinsic.unwrap_or(false)
                && field
                    .relation
                    .as_ref()
                    .is_some_and(|rel| rel.entity == identity);
            if points_at_identity {
                out.push((def.name.to_lowercase(), name.clone()));
                if let Some(coll) = &def.collection {
                    let cols = declared_by_collection.entry(coll.clone()).or_default();
                    if !cols.contains(name) {
                        cols.push(name.clone());
                    }
                }
            }
        }
    }
    // Self-identity: the `[identity]` entity's OWN `id` is the owner column when
    // its declared access policy compares it directly to `@user.id`.
    if let Some(def) = defs.iter().find(|d| d.name == identity) {
        if policies_self_identify(def) {
            let key = (identity.to_lowercase(), "id".to_string());
            if !out.contains(&key) {
                out.push(key);
            }
        }
    }
    // Policy-derived: a column an entity's OWN `@read`/`@update`/`@delete`
    // compares to `@user.id` is an owner column whether or not it is a
    // relation-to-identity FIELD — the codegen twin
    // (`access_policies.rs::owner_fields`, same reader) already finds this
    // shape (a policy naming a non-relation field), which the type-only scan
    // above cannot. `@create` is excluded — it compares the row being
    // written, which says nothing about what a fixture should already own.
    // Deduped against the type-derived set above (a relation field whose
    // policy also names it, e.g. `managerId`, is not double-counted).
    for def in &defs {
        for policy in [&def.update_policy, &def.delete_policy]
            .into_iter()
            .chain(std::iter::once(&def.read_policy))
            .flatten()
        {
            for field in owner_columns_from_policy(policy) {
                let key = (def.name.to_lowercase(), field);
                if !out.contains(&key) {
                    out.push(key);
                }
            }
        }
    }
    // Collection mirror of entity-access policy inheritance (JS twin:
    // `identityOwners.ts`): an entity sharing a collection with a declaring
    // sibling is scoped by that sibling's `@read`, so its same-named column IS
    // the owner column the policy compares — an imported atom's entity cannot
    // declare the relation itself (it does not know the app's identity entity).
    for def in &defs {
        let Some(coll) = &def.collection else {
            continue;
        };
        let Some(cols) = declared_by_collection.get(coll) else {
            continue;
        };
        for col in cols {
            let key = (def.name.to_lowercase(), col.clone());
            if out.contains(&key) {
                continue;
            }
            if def.fields.iter().any(|f| f.name.as_deref() == Some(col)) {
                out.push(key);
            }
        }
    }
    out
}

/// Assign the viewer every other row of each declared owner column.
///
/// Ownership-scoped views ("only my bookings") read `@user.id` against one of
/// these columns; against synthesized rows that never mention the viewer, a
/// correct filter and a broken one both render empty. Every *other* row, so a
/// second persona still sees a different, non-empty set.
/// Assign `viewer_id` to every declared owner column on every OTHER row (`i % 2 == 0`),
/// so a persona sees a genuine mix of owned and un-owned rows. Index 0 is always
/// stamped, so a single-row slice (the verify harness's synthetic row) is owned.
pub fn stamp_owner(
    rows: &mut [Value],
    entity_name: &str,
    viewer_id: Option<&str>,
    owner_fields: &[(String, String)],
) {
    let Some(viewer_id) = viewer_id else {
        return;
    };
    let entity_key = entity_name.to_lowercase();
    let cols: Vec<&str> = owner_fields
        .iter()
        .filter(|(entity, _)| *entity == entity_key)
        .map(|(_, field)| field.as_str())
        .collect();
    if cols.is_empty() {
        return;
    }
    for (i, row) in rows.iter_mut().enumerate() {
        if let Some(map) = row.as_object_mut() {
            for col in &cols {
                if stamps_row(col, i) {
                    map.insert((*col).to_string().into(), Value::string(viewer_id.to_string()));
                }
            }
        }
    }
    tracing::debug!(
        target: crate::logging::ns::SEED,
        entity = %entity_name,
        viewer = %viewer_id,
        columns = ?cols,
        "seed:owner-stamped",
    );
}

/// Which seeded rows the viewer owns through `column`: every other row, except a
/// self-identity `id` column, where the viewer is exactly one row (the first).
pub fn stamps_row(column: &str, index: usize) -> bool {
    if column == "id" {
        index == 0
    } else {
        index.is_multiple_of(2)
    }
}