//! Row-level entity access — the single owner of every declared `@read`/
//! `@create`/`@update`/`@delete` directive AND the call-site fetch `filter:`.
//!
//! An access directive is an S-expression predicate evaluated once per row
//! with `@entity` rebound to the row under test (or, for `@create`, the
//! incoming data — no row exists yet) and `@user` bound to the viewer.
//! Every `FetchHandler`/persist path applies it through here, so the
//! ownership-scoped result ("only my tickets") narrows identically no matter
//! which handler served it, and every trait-issued mutation is checked no
//! matter which trait issues it — the un-bypassable twin of a call-site
//! `filter:`, which sibling traits can skip simply by not declaring one
//! (R-FETCH-SCOPE-SIBLING-BYPASS).

use crate::effects::PersistOperation;
use crate::evaluator::{BindingContext, Evaluator};
use crate::schema::{EntityDefinition, FieldDefinition, SExpression};
use crate::{OrbitalSchema, Value};
use std::collections::HashMap;

/// The bare sigil root this module's `@user.<f>` readers are about.
const USER_SIGIL: &str = "@user.";

/// Comparison operators whose two operands are compared for equality. `=` and
/// `==` both occur in committed `.orb` files. Shared with `orbital-compiler`'s
/// `phases::validation::user_identity::check_comparison` (which re-exports
/// this constant rather than declaring its own) and the orbital-import
/// role-literal rewrite (`inline/rewrite.rs::rewrite_role_literals`, which
/// must stay shape-identical to `check_comparison`).
pub const EQUALITY_OPS: &[&str] = &["=", "==", "!=", "!==", "eq", "neq"];

/// The declared vocabulary for an identity field, following `items` for the
/// array form. `roles : ["a" | "b"]` keeps the union on the ELEMENT, so a
/// multi-valued role field must be read through `items`. Moved here from
/// `orbital-compiler`'s `user_identity` module (re-exported there) so
/// [`role_satisfying_policy`] and `check_comparison` share one reader.
pub fn vocabulary(field: &FieldDefinition) -> Option<&Vec<String>> {
    if let Some(values) = field.values.as_ref().filter(|v| !v.is_empty()) {
        return Some(values);
    }
    field
        .items
        .as_ref()
        .and_then(|item| item.values.as_ref())
        .filter(|v| !v.is_empty())
}

/// Look a field up on the identity entity by name. Moved here from
/// `orbital-compiler`'s `user_identity` module (re-exported there).
pub fn identity_field<'a>(
    identity: &'a EntityDefinition,
    name: &str,
) -> Option<&'a FieldDefinition> {
    identity
        .fields
        .iter()
        .find(|f| f.name.as_deref() == Some(name))
}

/// `@user.role` → `Some("role")`. Only a direct field read; `@user` alone and
/// deeper paths (`@user.profile.x`) are not field claims this rule can judge.
/// Moved here from `orbital-compiler`'s `user_identity` module (re-exported
/// there).
pub fn user_field_of(s: &str) -> Option<&str> {
    let rest = s.strip_prefix(USER_SIGIL)?;
    if rest.is_empty() || rest.contains('.') || rest.contains('[') {
        return None;
    }
    Some(rest)
}

/// Every literal an expression compares against `@user.<field>` (any field),
/// keyed by field name — the `SExpression` twin of `orbital-compiler`'s
/// `validation::user_identity::check_comparison` (which walks the identical
/// shapes over `serde_json::Value`; the two must stay shape-identical) and
/// the JS `collectUserFieldLiterals`
/// (`packages/almadar-core/src/mock/identityOwners.ts`) this function is the
/// Rust twin of: an equality node ([`EQUALITY_OPS`]) or an `array/includes`
/// node, walked transparently through `and`/`or`/`not` and any other
/// combinator.
///
/// `array/includes` is directional, but the sigil can sit on EITHER side:
/// `(array/includes @user.roles "ghost")` checks a literal against a
/// multi-valued field's own vocabulary (field is the haystack, position 0);
/// the Stage-B orbital-import role rewrite instead produces
/// `(array/includes ["owner","project_manager"] @user.role)` — a literal
/// ARRAY haystack with the sigil as the needle, position 1. So the sigil is
/// located BY CONTENT, never position: check position 0 first, fall back to
/// position 1.
///
/// A collected literal is a CANDIDATE a viewer might satisfy the expression
/// with, never a proof — see [`role_satisfying_policy`]'s doc.
pub fn collect_user_field_literals(expr: &SExpression, out: &mut HashMap<String, Vec<String>>) {
    match expr {
        SExpression::Array(items) => {
            if let Some(SExpression::String(op)) = items.first() {
                let args = &items[1..];
                let is_includes = op == "array/includes";
                if args.len() >= 2 && (EQUALITY_OPS.contains(&op.as_str()) || is_includes) {
                    let (sigil_side, literal_side): (&[SExpression], &[SExpression]) =
                        if is_includes {
                            let first_is_sigil = matches!(
                                &args[0],
                                SExpression::String(s) if user_field_of(s).is_some()
                            );
                            if first_is_sigil {
                                (&args[..1], &args[1..2])
                            } else {
                                (&args[1..2], &args[..1])
                            }
                        } else {
                            (args, args)
                        };
                    let field = sigil_side.iter().find_map(|a| match a {
                        SExpression::String(s) => user_field_of(s),
                        _ => None,
                    });
                    if let Some(field) = field {
                        let literals: Vec<&SExpression> = if is_includes {
                            match literal_side.first() {
                                Some(SExpression::Array(items)) => items.iter().collect(),
                                Some(other) => vec![other],
                                None => Vec::new(),
                            }
                        } else {
                            literal_side.iter().collect()
                        };
                        for literal in literals {
                            if let SExpression::String(s) = literal {
                                if !s.starts_with('@') {
                                    let set = out.entry(field.to_string()).or_default();
                                    if !set.contains(s) {
                                        set.push(s.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for item in items {
                collect_user_field_literals(item, out);
            }
        }
        SExpression::Object(map) => {
            for value in map.values() {
                collect_user_field_literals(value, out);
            }
        }
        _ => {}
    }
}

/// `@user.role` field name `role_satisfying_policy` resolves against.
const ROLE_FIELD: &str = "role";

/// The `@user.role` value that satisfies `entity`'s declared `kind`
/// (`create`/`update`/`delete`; `read` carries no mutation directive here)
/// access policy, chosen from the schema's `[identity]` entity's own
/// declared `role` vocabulary — Rust twin of JS `roleSatisfyingPolicy`
/// (`packages/almadar-core/src/mock/identityOwners.ts`). Never a guess: only
/// a literal BOTH (a) compared against `role` somewhere in the policy (see
/// [`collect_user_field_literals`]) AND (b) a declared member of the
/// identity entity's own `role` vocabulary (see [`vocabulary`]) is a
/// candidate. Deterministic — the first vocabulary value, in declaration
/// order, that is also a candidate.
///
/// `None` when: the schema declares no `[identity]` entity, no entity named
/// `entity`, `entity` declares no policy for `kind`, the identity entity
/// declares no `role` vocabulary, or no declared role appears anywhere in
/// the policy — a genuine finding (an unsatisfiable policy), never forced.
/// `orbital-verify`'s `verify_with_harness` calls this per persist step so a
/// role-only policy is walked with a viewer that can actually pass it,
/// instead of being bucketed as an access denial under the roster's default
/// persona.
pub fn role_satisfying_policy(
    schema: &OrbitalSchema,
    entity: &str,
    kind: PersistOperation,
) -> Option<(String, String)> {
    let identity = super::seed::identity_entity_def(schema)?;
    let def = super::seed::inline_entity_defs(schema)
        .into_iter()
        .find(|d| d.name == entity)?;
    let policy = match kind {
        PersistOperation::Create | PersistOperation::Upsert => def.create_policy.as_ref(),
        PersistOperation::Update => def.update_policy.as_ref(),
        PersistOperation::Delete => def.delete_policy.as_ref(),
        PersistOperation::Read => None,
    }?;
    let field = identity_field(identity, ROLE_FIELD)?;
    let vocab = vocabulary(field)?;
    let mut literals: HashMap<String, Vec<String>> = HashMap::new();
    collect_user_field_literals(policy, &mut literals);
    let accepted = literals.get(ROLE_FIELD)?;
    vocab
        .iter()
        .find(|v| accepted.contains(v))
        .map(|v| (ROLE_FIELD.to_string(), v.clone()))
}

/// An entity's four declared access directives, resolved from its schema
/// definition. Any of the four may be absent — an absent directive means
/// "no restriction" for that operation, exactly like an absent fetch `filter:`.
#[derive(Debug, Clone, Default)]
pub struct EntityAccessPolicies {
    pub read: Option<Value>,
    pub create: Option<Value>,
    pub update: Option<Value>,
    pub delete: Option<Value>,
    /// The `persistent:` collection this entity binds, when it has one. Access
    /// is a property of the DATA, so this is what policies are ultimately keyed
    /// by — see `resolve_collection_inheritance`.
    pub collection: Option<String>,
    /// True when the directives above were adopted from a sibling entity on the
    /// same collection rather than declared by this entity. Kept distinct so
    /// re-resolution never mistakes an inherited policy for a declared one and
    /// propagates it onward.
    pub inherited: bool,
    /// Names of this entity's `persist create`-required columns — see
    /// [`required_create_field_names`]. A property of the entity's OWN field
    /// declarations, never inherited from a sibling on the same collection.
    pub required_create_fields: Vec<String>,
    /// Names of every field the entity declares required (`!`) or with a default: never missing, so an ordering comparison on one
    /// reads the same in a predicate and in the evaluator.
    pub required_fields: Vec<String>,
    /// Names of this entity's `@intrinsic` fields — the trait's own frame, never a persisted column.
    pub intrinsic_fields: Vec<String>,
}

impl EntityAccessPolicies {
    /// Whether **this entity's author** declared any of the four directives.
    /// An entity that declared none is a candidate to inherit its collection's
    /// policy (`R-SHADOW-ENTITY-SHARES-COLLECTION-DIRECTIVES-DONT-APPLY`); one
    /// that declared even a single directive keeps exactly what it declared.
    pub fn is_declared(&self) -> bool {
        !self.inherited
            && (self.read.is_some()
                || self.create.is_some()
                || self.update.is_some()
                || self.delete.is_some())
    }
}

/// Propagate each collection's declared access policy to every other entity
/// bound to that same collection which declares none of its own.
///
/// A `persistent:` collection is one table, and two entity declarations over one
/// table cannot honestly carry different visibility rules. Keying policies by
/// entity NAME alone let an organism declare `@read` on its own entity and still
/// ship an ALLOW-ALL list wherever a page rendered through a std-atom trait that
/// fetched the ATOM's entity on the same collection — authorization theatre on
/// 36 of 54 app organisms (`R-SHADOW-ENTITY-SHARES-COLLECTION-DIRECTIVES-DONT-APPLY`).
///
/// Runs over the WHOLE accumulated table after every registration, not just the
/// schema being registered: the organism's entity and the atom's entity arrive
/// in different `register_schema` calls, so a within-schema pass would never see
/// them together.
///
/// Only ever ADDS a restriction — an entity with its own directives keeps them,
/// and an entity whose collection has no declared policy is left untouched.
/// Conflicting declarations on one collection are rejected at validate time by
/// `ORB_S_COLLECTION_POLICY_CONFLICT`, so at most one declaration per collection
/// reaches here.
pub fn resolve_collection_inheritance(table: &mut EntityAccessTable) {
    let mut by_collection: HashMap<String, EntityAccessPolicies> = HashMap::new();
    for policies in table.values() {
        if !policies.is_declared() {
            continue;
        }
        if let Some(collection) = &policies.collection {
            by_collection
                .entry(collection.clone())
                .or_insert_with(|| policies.clone());
        }
    }

    for (name, policies) in table.iter_mut() {
        if policies.is_declared() {
            continue;
        }
        let Some(source) = policies
            .collection
            .as_ref()
            .and_then(|c| by_collection.get(c))
        else {
            continue;
        };
        tracing::debug!(
            entity = %name,
            collection = ?policies.collection,
            "adopting the access policy declared on this entity's collection",
        );
        policies.read = source.read.clone();
        policies.create = source.create.clone();
        policies.update = source.update.clone();
        policies.delete = source.delete.clone();
        policies.inherited = true;
    }
}

/// Schema-derived policy table, `entity name -> its four directives`. Built
/// once at schema registration (`RuntimeKernel::register_schema`) and shared
/// with both the fetch and persist paths.
pub type EntityAccessTable = HashMap<String, EntityAccessPolicies>;

/// Retain only the rows BOTH the declared `@read` policy and the call-site
/// `filter:` accept — the policy can only narrow what the call site sees,
/// never widen it. A no-op when neither is declared.
///
/// A predicate that fails to evaluate drops its row: a filter/policy that
/// cannot be evaluated must never widen the result set.
pub fn apply_read_access(
    rows: &mut Vec<Value>,
    policy: Option<&Value>,
    filter: Option<&Value>,
    ctx: &BindingContext,
    entity_type: &str,
) {
    if policy.is_none() && filter.is_none() {
        return;
    }
    let evaluator = Evaluator::new();
    let before = rows.len();
    let predicates: Vec<SExpression> = [policy, filter]
        .into_iter()
        .flatten()
        .map(|predicate| predicate.clone().into())
        .collect();
    let mut row_ctx = ctx.clone();
    let candidates = std::mem::take(rows);
    for row in candidates {
        let shared = std::sync::Arc::new(row);
        row_ctx.entity = std::sync::Arc::clone(&shared);
        let mut keep = true;
        for predicate in &predicates {
            match evaluator.evaluate(predicate, &row_ctx) {
                Ok(v) if v.is_truthy() => continue,
                Ok(_) => keep = false,
                Err(err) => {
                    tracing::warn!(
                        entity_type,
                        error = %err,
                        "read access predicate failed to evaluate; row dropped"
                    );
                    keep = false;
                }
            }
            break;
        }
        row_ctx.entity = std::sync::Arc::clone(&ctx.entity);
        if keep {
            rows.push(std::sync::Arc::try_unwrap(shared).unwrap_or_else(|held| (*held).clone()));
        }
    }
    tracing::debug!(
        target: crate::logging::ns::RUNTIME_FETCH,
        entity = %entity_type,
        before,
        after = rows.len(),
        "fetch:read-access-applied",
    );
}

/// The one fetch-scoping owner every `FetchHandler` calls: `@read` ∧ `filter:`, then offset, then limit.
/// Returns the row count before offset/limit (the fetch payload's `totalCount`).
#[allow(clippy::too_many_arguments)]
pub fn scope_fetched_rows(
    rows: &mut Vec<Value>,
    policy: Option<&Value>,
    filter: Option<&Value>,
    limit: Option<usize>,
    offset: Option<usize>,
    ctx: &BindingContext,
    entity_type: &str,
) -> usize {
    apply_read_access(rows, policy, filter, ctx, entity_type);
    let total = rows.len();
    if let Some(off) = offset.filter(|o| *o > 0) {
        if off >= rows.len() {
            rows.clear();
        } else {
            rows.drain(..off);
        }
    }
    if let Some(lim) = limit {
        rows.truncate(lim);
    }
    total
}

/// Check a declared `@create`/`@update`/`@delete` directive against the row
/// the operation targets. `row` is the incoming data for `Create` (no row
/// exists yet) or the EXISTING row fetched fresh for `Update`/`Delete`.
/// `Read` is not a valid argument here — reads go through [`apply_read_access`].
///
/// Returns `Ok(())` when no policy is declared (today's behavior, unchanged)
/// or the policy evaluates truthy; `Err` denies the mutation, including when
/// the predicate fails to evaluate (fail-closed, same rule as reads).
pub fn check_mutation_access(
    operation: PersistOperation,
    row: &Value,
    policy: Option<&Value>,
    ctx: &BindingContext,
    entity_type: &str,
) -> Result<(), String> {
    let Some(policy) = policy else {
        return Ok(());
    };
    let predicate: SExpression = policy.clone().into();
    let mut row_ctx = ctx.clone();
    row_ctx.set_entity(row.clone());
    let evaluator = Evaluator::new();
    match evaluator.evaluate(&predicate, &row_ctx) {
        Ok(v) if v.is_truthy() => Ok(()),
        Ok(_) => Err(format!(
            "@{operation:?} denied: the declared access policy for `{entity_type}` rejected this row"
        )),
        Err(err) => {
            tracing::warn!(
                entity_type,
                operation = ?operation,
                error = %err,
                "mutation access predicate failed to evaluate; mutation denied"
            );
            Err(format!(
                "@{operation:?} denied: the declared access policy for `{entity_type}` could not be evaluated ({err})"
            ))
        }
    }
}

/// Names of the entity's `@intrinsic` fields.
pub fn intrinsic_field_names(entity: &EntityDefinition) -> Vec<String> {
    entity
        .fields
        .iter()
        .filter(|f| f.intrinsic == Some(true))
        .filter_map(|f| f.name.clone())
        .collect()
}

/// Names of `entity`'s `persist create`-required columns: a field declaring
/// `required: true`, excluding [`crate::schema::FRAMEWORK_STAMPED_FIELDS`],
/// any `@intrinsic` field, any field declaring a `default` (the store fills
/// it in), and any field carrying `merged_from` (a field the inline phase's
/// rebind merge added from an imported atom's entity, never authored by this
/// entity's own writer — the atom's own effects satisfy its own `!`, and
/// requiring it of every host `persist create` would fail on a column the
/// host never declared) — the definition shared with `@almadar/runtime`'s
/// `missingRequiredFields`.
pub fn required_create_field_names(entity: &EntityDefinition) -> Vec<String> {
    entity
        .fields
        .iter()
        .filter_map(|f| {
            let name = f.name.as_ref()?;
            if f.required != Some(true) {
                return None;
            }
            if crate::schema::FRAMEWORK_STAMPED_FIELDS.contains(&name.as_str()) {
                return None;
            }
            if f.intrinsic == Some(true) {
                return None;
            }
            if f.default.is_some() {
                return None;
            }
            if f.merged_from.is_some() {
                return None;
            }
            Some(name.clone())
        })
        .collect()
}

/// Which of `required` are missing from `data` — key absent, or `null`/`""`.
/// A no-op (`[]`) when `required` is empty.
pub fn missing_required_fields(required: &[String], data: &Value) -> Vec<String> {
    let obj = data.as_object();
    required
        .iter()
        .filter(|name| {
            let value = obj.and_then(|o| o.get(name.as_str()));
            match value {
                None => true,
                Some(Value::Null) => true,
                Some(Value::String(s)) if s.is_empty() => true,
                _ => false,
            }
        })
        .cloned()
        .collect()
}

/// Does `msg` carry the distinctive shape of a [`check_mutation_access`]
/// denial? Lets a caller (e.g. `orbital-verify`'s persona-parameterised run,
/// task #129) bucket an access-policy denial separately from a genuine effect
/// failure without hand-rolling a fragile match against this module's private
/// wording.
pub fn is_access_denied_error(msg: &str) -> bool {
    msg.contains(" denied: the declared access policy for ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::BindingContextBuilder;
    use crate::evaluator::ValueMap;

    fn row(id: &str, owner: &str) -> Value {
        let mut m = ValueMap::new();
        m.insert("id".to_string(), Value::String(id.to_string()));
        m.insert("memberId".to_string(), Value::String(owner.to_string()));
        Value::Object(m)
    }

    fn viewer(id: &str) -> Value {
        let mut m = ValueMap::new();
        m.insert("id".to_string(), Value::String(id.to_string()));
        Value::Object(m)
    }

    /// `["=", "@entity.memberId", "@user.id"]` — the ownership-scoping shape.
    fn owner_predicate() -> Value {
        Value::Array(vec![
            Value::String("=".to_string()),
            Value::String("@entity.memberId".to_string()),
            Value::String("@user.id".to_string()),
        ])
    }

    fn status_open_predicate() -> Value {
        Value::Array(vec![
            Value::String("=".to_string()),
            Value::String("@entity.status".to_string()),
            Value::String("open".to_string()),
        ])
    }

    fn row_with_status(id: &str, owner: &str, status: &str) -> Value {
        let mut m = ValueMap::new();
        m.insert("id".to_string(), Value::String(id.to_string()));
        m.insert("memberId".to_string(), Value::String(owner.to_string()));
        m.insert("status".to_string(), Value::String(status.to_string()));
        Value::Object(m)
    }

    #[test]
    fn scopes_rows_to_the_viewer() {
        let mut rows = vec![
            row("r1", "member-1"),
            row("r2", "member-2"),
            row("r3", "member-1"),
        ];
        let ctx = BindingContextBuilder::new()
            .user(viewer("member-1"))
            .build();
        apply_read_access(
            &mut rows,
            Some(&owner_predicate()),
            None,
            &ctx,
            "RosterEntry",
        );
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn a_different_viewer_gets_the_complement() {
        let mut rows = vec![
            row("r1", "member-1"),
            row("r2", "member-2"),
            row("r3", "member-1"),
        ];
        let ctx = BindingContextBuilder::new()
            .user(viewer("member-2"))
            .build();
        apply_read_access(
            &mut rows,
            Some(&owner_predicate()),
            None,
            &ctx,
            "RosterEntry",
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn no_viewer_scopes_to_nothing_rather_than_everything() {
        let mut rows = vec![row("r1", "member-1"), row("r2", "member-2")];
        let ctx = BindingContextBuilder::new().build();
        apply_read_access(
            &mut rows,
            Some(&owner_predicate()),
            None,
            &ctx,
            "RosterEntry",
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn absent_policy_and_filter_keeps_every_row() {
        let mut rows = vec![row("r1", "member-1"), row("r2", "member-2")];
        let ctx = BindingContextBuilder::new()
            .user(viewer("member-1"))
            .build();
        apply_read_access(&mut rows, None, None, &ctx, "RosterEntry");
        assert_eq!(rows.len(), 2);
    }

    /// The policy and the call-site filter both apply — the filter can only
    /// narrow further, never widen past what the policy already allows.
    #[test]
    fn policy_and_filter_and_together() {
        let mut rows = vec![
            row_with_status("r1", "member-1", "open"),
            row_with_status("r2", "member-1", "closed"),
            row_with_status("r3", "member-2", "open"),
        ];
        let ctx = BindingContextBuilder::new()
            .user(viewer("member-1"))
            .build();
        apply_read_access(
            &mut rows,
            Some(&owner_predicate()),
            Some(&status_open_predicate()),
            &ctx,
            "RosterEntry",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get_path("id"),
            Some(&Value::String("r1".to_string()))
        );
    }

    #[test]
    fn create_against_incoming_data_is_checked() {
        let ctx = BindingContextBuilder::new()
            .user(viewer("member-1"))
            .build();
        let incoming = row("new", "member-1");
        assert!(check_mutation_access(
            PersistOperation::Create,
            &incoming,
            Some(&owner_predicate()),
            &ctx,
            "Ticket"
        )
        .is_ok());

        let incoming_wrong_owner = row("new", "member-2");
        assert!(check_mutation_access(
            PersistOperation::Create,
            &incoming_wrong_owner,
            Some(&owner_predicate()),
            &ctx,
            "Ticket"
        )
        .is_err());
    }

    #[test]
    fn update_against_the_existing_row_is_checked() {
        let ctx = BindingContextBuilder::new()
            .user(viewer("member-2"))
            .build();
        let existing = row("r1", "member-1");
        assert!(check_mutation_access(
            PersistOperation::Update,
            &existing,
            Some(&owner_predicate()),
            &ctx,
            "Ticket"
        )
        .is_err());
    }

    #[test]
    fn no_policy_allows_the_mutation() {
        let ctx = BindingContextBuilder::new()
            .user(viewer("member-1"))
            .build();
        let row_data = row("r1", "member-2");
        assert!(
            check_mutation_access(PersistOperation::Delete, &row_data, None, &ctx, "Ticket")
                .is_ok()
        );
    }

    #[test]
    fn denial_errors_carry_the_shared_detectable_shape() {
        let ctx = BindingContextBuilder::new()
            .user(viewer("member-2"))
            .build();
        let err = check_mutation_access(
            PersistOperation::Delete,
            &row("r1", "member-1"),
            Some(&owner_predicate()),
            &ctx,
            "Ticket",
        )
        .unwrap_err();
        assert!(is_access_denied_error(&err));
        assert!(!is_access_denied_error("some other failure entirely"));
    }
}

/// `role_satisfying_policy` — the twin `orbital-verify` calls per persist step
/// so a role-only policy is walked with a viewer that can actually pass it.
/// JS twin: `identity-owner-fields.test.ts`'s `roleSatisfyingPolicy` block.
#[cfg(test)]
mod role_satisfying_policy_tests {
    use super::*;
    use serde_json::json;

    fn schema_with_employee_policy(update_policy: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "EmployeeOrbital",
                "entity": {
                    "name": "Employee",
                    "identity": true,
                    "fields": [
                        { "name": "id", "type": "string" },
                        { "name": "role", "type": "string", "values": ["employee", "approver"] }
                    ]
                },
                "traits": []
            }, {
                "name": "TimesheetOrbital",
                "entity": {
                    "name": "Timesheet",
                    "update_policy": update_policy,
                    "fields": [{ "name": "id", "type": "string" }]
                },
                "traits": []
            }]
        }))
        .expect("schema fixture must deserialize")
    }

    #[test]
    fn picks_the_declared_role_literal_for_a_strict_equality_policy() {
        let schema = schema_with_employee_policy(json!(["=", "@user.role", "approver"]));
        assert_eq!(
            role_satisfying_policy(&schema, "Timesheet", PersistOperation::Update),
            Some(("role".to_string(), "approver".to_string()))
        );
    }

    #[test]
    fn picks_deterministically_declaration_order_when_more_than_one_role_satisfies() {
        let schema = schema_with_employee_policy(json!([
            "or",
            ["=", "@user.role", "employee"],
            ["=", "@user.role", "approver"]
        ]));
        assert_eq!(
            role_satisfying_policy(&schema, "Timesheet", PersistOperation::Update),
            Some(("role".to_string(), "employee".to_string()))
        );
    }

    #[test]
    fn returns_none_when_no_roster_role_satisfies_the_policy() {
        let schema = schema_with_employee_policy(json!(["=", "@user.role", "owner"]));
        assert_eq!(
            role_satisfying_policy(&schema, "Timesheet", PersistOperation::Update),
            None
        );
    }

    #[test]
    fn returns_none_when_the_entity_declares_no_policy_for_kind() {
        let schema = schema_with_employee_policy(json!(["=", "@user.role", "approver"]));
        assert_eq!(
            role_satisfying_policy(&schema, "Timesheet", PersistOperation::Delete),
            None
        );
    }

    /// Project Friday's imported `Timesheet.@delete` after `roles { approver:
    /// [owner, project_manager] }`: a reversed `array/includes` — literal-array
    /// haystack first, `@user.role` needle second.
    #[test]
    fn picks_a_role_from_a_reversed_array_includes_literal_array_haystack() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "EmployeeOrbital",
                "entity": {
                    "name": "Employee",
                    "identity": true,
                    "fields": [
                        { "name": "id", "type": "string" },
                        { "name": "role", "type": "string", "values": ["owner", "project_manager"] }
                    ]
                },
                "traits": []
            }, {
                "name": "TimesheetOrbital",
                "entity": {
                    "name": "Timesheet",
                    "delete_policy": ["array/includes", ["owner", "project_manager"], "@user.role"],
                    "fields": [{ "name": "id", "type": "string" }]
                },
                "traits": []
            }]
        }))
        .expect("schema fixture must deserialize");
        assert_eq!(
            role_satisfying_policy(&schema, "Timesheet", PersistOperation::Delete),
            Some(("role".to_string(), "owner".to_string()))
        );
    }
}
