//! An access policy governs the COLLECTION, not the entity declaration.
//!
//! `R-SHADOW-ENTITY-SHARES-COLLECTION-DIRECTIVES-DONT-APPLY`: the policy table
//! was keyed by entity NAME, so an organism could declare `@read` on its own
//! entity and still ship an ALLOW-ALL list wherever the page rendered through a
//! std-atom trait that fetched the ATOM's entity bound to the same `persistent:`
//! collection. Measured on `std-customer-success`: `HealthScoreSearch`
//! [healthscores] declares `@read (or manager, assignedCsm == @user.id)`, while
//! `std-health-score.lolo`'s `HealthScore` [healthscores] declares nothing — and
//! `/health-scores` renders through the atom, so per-persona row counts came
//! back 6 vs 6 while the organism's own `/cs-accounts` page correctly returned
//! 3 vs 6. 36 of 54 app organisms share a collection this way.
//!
//! The two entities arrive in **different** `register_schema` calls (the atom's
//! traits resolve from the registry rather than being inlined), which is why
//! resolution runs over the whole accumulated table.

use orbital_core::ir::{OirEntity, OirExpr, OirModule};
use orbital_core::runtime::entity_access::{
    resolve_collection_inheritance, EntityAccessPolicies, EntityAccessTable,
};
use orbital_core::Value;

fn policy(marker: &str) -> Value {
    Value::String(marker.to_string())
}

fn declared(collection: &str, read: &str) -> EntityAccessPolicies {
    EntityAccessPolicies {
        read: Some(policy(read)),
        create: None,
        update: None,
        delete: None,
        collection: Some(collection.to_string()),
        inherited: false,
        required_create_fields: Vec::new(),
        required_fields: Vec::new(),
        intrinsic_fields: Vec::new(),
    }
}

fn undeclared(collection: Option<&str>) -> EntityAccessPolicies {
    EntityAccessPolicies {
        read: None,
        create: None,
        update: None,
        delete: None,
        collection: collection.map(str::to_string),
        inherited: false,
        required_create_fields: Vec::new(),
        required_fields: Vec::new(),
        intrinsic_fields: Vec::new(),
    }
}

fn table(entries: &[(&str, EntityAccessPolicies)]) -> EntityAccessTable {
    entries
        .iter()
        .map(|(name, p)| ((*name).to_string(), p.clone()))
        .collect()
}

/// The reported shape, verbatim.
#[test]
fn atom_entity_adopts_the_organisms_policy_on_the_shared_collection() {
    let mut t = table(&[
        ("HealthScoreSearch", declared("healthscores", "org-read")),
        ("HealthScore", undeclared(Some("healthscores"))),
    ]);
    resolve_collection_inheritance(&mut t);

    let atom = &t["HealthScore"];
    assert_eq!(
        atom.read.as_ref(),
        Some(&policy("org-read")),
        "the atom's entity must inherit the collection's declared @read"
    );
    assert!(atom.inherited, "and be marked inherited, not declared");
    assert!(
        !atom.is_declared(),
        "an inherited policy must never count as a declaration"
    );

    // The declaring entity is untouched.
    assert!(t["HealthScoreSearch"].is_declared());
    assert!(!t["HealthScoreSearch"].inherited);
}

/// Inheritance can only ever ADD a restriction.
#[test]
fn an_entity_with_its_own_directives_keeps_them() {
    let mut t = table(&[
        ("Ticket", declared("tickets", "organism-read")),
        ("ListTicket", declared("tickets", "own-read")),
    ]);
    resolve_collection_inheritance(&mut t);

    assert_eq!(t["ListTicket"].read.as_ref(), Some(&policy("own-read")));
    assert_eq!(t["Ticket"].read.as_ref(), Some(&policy("organism-read")));
}

/// Sharing a collection is not by itself a defect, and an unrelated collection
/// must not pick anything up. `std-retail-pos` shares `sales` with
/// `std-retail-store.lolo` and still measured a real 3-vs-6 delta.
#[test]
fn unrelated_collections_are_untouched() {
    let mut t = table(&[
        ("Sale", declared("sales", "sales-read")),
        ("Invoice", undeclared(Some("invoices"))),
        ("Ephemeral", undeclared(None)),
    ]);
    resolve_collection_inheritance(&mut t);

    assert!(t["Invoice"].read.is_none(), "different collection");
    assert!(!t["Invoice"].inherited);
    assert!(t["Ephemeral"].read.is_none(), "no collection at all");
    assert!(!t["Ephemeral"].inherited);
}

/// Re-resolution is idempotent, and an inherited policy is never treated as a
/// declaration that could propagate onward. Registrations are incremental, so
/// this runs many times over a growing table.
#[test]
fn re_resolution_is_idempotent_and_inherited_policies_do_not_propagate() {
    let mut t = table(&[
        ("Owner", declared("things", "read-a")),
        ("Shadow", undeclared(Some("things"))),
    ]);
    resolve_collection_inheritance(&mut t);
    let after_first = t["Shadow"].clone();

    // A later registration adds another shadow on the same collection.
    t.insert("LateShadow".to_string(), undeclared(Some("things")));
    resolve_collection_inheritance(&mut t);

    assert_eq!(t["Shadow"].read, after_first.read, "stable across re-runs");
    assert_eq!(
        t["LateShadow"].read.as_ref(),
        Some(&policy("read-a")),
        "an entity registered later still inherits"
    );

    // Drop the declaring entity: the inherited copies must not become the
    // source of truth for anything new.
    t.remove("Owner");
    t.insert("Newest".to_string(), undeclared(Some("things")));
    resolve_collection_inheritance(&mut t);
    assert!(
        t["Newest"].read.is_none(),
        "an inherited policy must not seed further inheritance"
    );
}

// ── The compiled path's twin ────────────────────────────────────────────────
// `OirModule::resolve_collection_policy_inheritance` is what makes the emitted
// server agree with the kernel above. The TypeScript shell emits one policy
// const per entity that CARRIES a directive, so an atom entity left undeclared
// emits none at all and its routes serve ALLOW-ALL.

fn oir_entity(name: &str, collection: &str, read: Option<&str>) -> OirEntity {
    let mut e = OirEntity::new(name);
    e.collection = collection.to_string();
    e.read_policy = read.map(OirExpr::string);
    e
}

fn oir_module(entities: Vec<OirEntity>) -> OirModule {
    let mut m = OirModule::new("test", "1.0.0");
    m.entities = entities;
    m
}

#[test]
fn oir_atom_entity_adopts_the_organisms_policy_on_the_shared_collection() {
    let mut m = oir_module(vec![
        oir_entity("HealthScoreSearch", "healthscores", Some("org-read")),
        oir_entity("HealthScore", "healthscores", None),
    ]);
    m.resolve_collection_policy_inheritance();

    assert_eq!(
        m.entities[1].read_policy,
        Some(OirExpr::string("org-read")),
        "the spliced-in atom entity must emit the collection's policy const"
    );
    assert_eq!(m.entities[0].read_policy, Some(OirExpr::string("org-read")));
}

#[test]
fn oir_inheritance_only_ever_adds_a_restriction() {
    let mut m = oir_module(vec![
        oir_entity("Ticket", "tickets", Some("organism-read")),
        oir_entity("ListTicket", "tickets", Some("own-read")),
        oir_entity("Invoice", "invoices", None),
    ]);
    m.resolve_collection_policy_inheritance();

    assert_eq!(
        m.entities[1].read_policy,
        Some(OirExpr::string("own-read")),
        "an entity with its own directive keeps exactly it"
    );
    assert!(
        m.entities[2].read_policy.is_none(),
        "an unrelated collection picks up nothing"
    );
}

#[test]
fn oir_a_policy_free_module_is_untouched() {
    let mut m = oir_module(vec![
        oir_entity("A", "things", None),
        oir_entity("B", "things", None),
    ]);
    m.resolve_collection_policy_inheritance();

    assert!(m.entities.iter().all(|e| !e.declares_access_policy()));
}

#[test]
fn oir_one_declared_directive_blocks_adoption_of_the_other_three() {
    let mut declaring = oir_entity("Owner", "things", Some("owner-read"));
    declaring.create_policy = Some(OirExpr::string("owner-create"));
    let mut partial = oir_entity("Partial", "things", None);
    partial.update_policy = Some(OirExpr::string("partial-update"));

    let mut m = oir_module(vec![declaring, partial]);
    m.resolve_collection_policy_inheritance();

    let p = &m.entities[1];
    assert_eq!(p.update_policy, Some(OirExpr::string("partial-update")));
    assert!(
        p.read_policy.is_none() && p.create_policy.is_none(),
        "an author who declared one directive chose the others unrestricted"
    );
}
