//! `expects` derivation (docs/Almadar_LOLO_Expects_Proposal.md §7): ported case for case from
//! `@almadar/core`'s `derive-expectations.test.ts`, the derivation's former owner, plus the
//! event arm.

use orbital_core::expects::{derive_expectations, merge_expectation_shape, DerivedExpectations};
use serde_json::{json, Value};

fn customer_fields() -> Value {
    json!([
        { "name": "id", "type": "string", "required": true },
        { "name": "name", "type": "string", "required": true },
        { "name": "role", "type": "enum", "values": ["customer", "store-manager"], "default": "customer" }
    ])
}

fn order_record_fields() -> Value {
    json!([
        { "name": "id", "type": "string", "required": true },
        { "name": "status", "type": "enum", "values": ["placed", "shipped"] },
        { "name": "totalAmount", "type": "number" },
        { "name": "customer", "type": "relation", "relation": { "entity": "Customer", "cardinality": "one" } }
    ])
}

fn build_schema() -> Value {
    let checkout_trait = json!({
        "name": "CheckoutWizard",
        "scope": "instance",
        "linkedEntity": "Checkout",
        "stateMachine": {
            "states": [{ "name": "editing", "isInitial": true }, { "name": "done" }],
            "events": [{ "key": "COMPLETE", "name": "Complete" }],
            "transitions": [{
                "from": "editing",
                "to": "done",
                "event": "COMPLETE",
                "guard": ["=", "@user.id", "@entity.customer"],
                "effects": [
                    ["persist", "create", "OrderRecord", { "status": "placed", "totalAmount": "@payload.total" }],
                    ["fetch", "OrderRecord", { "include": ["customer"] }],
                    ["persist", "update", "Checkout", { "done": true }]
                ]
            }]
        }
    });
    json!({
        "name": "fixture-shop",
        "orbitals": [
            { "name": "CustomerOrbital", "entity": { "name": "Customer", "identity": true, "fields": customer_fields() }, "traits": [], "pages": [] },
            {
                "name": "ProductOrbital",
                "entity": {
                    "name": "Product",
                    "fields": [
                        { "name": "id", "type": "string", "required": true },
                        { "name": "title", "type": "string" },
                        { "name": "owner", "type": "relation", "relation": { "entity": "Customer", "cardinality": "one" } }
                    ],
                    "create_policy": ["=", "@user.role", "store-manager"]
                },
                "traits": [], "pages": []
            },
            {
                "name": "CheckoutOrbital",
                "entity": { "name": "Checkout", "fields": [{ "name": "id", "type": "string", "required": true }, { "name": "customer", "type": "string" }] },
                "traits": [checkout_trait], "pages": []
            },
            {
                "name": "OrderRecordOrbital",
                "entity": { "name": "OrderRecord", "fields": order_record_fields(), "read_policy": ["=", "@user.role", "store-manager"] },
                "traits": [], "pages": []
            },
            {
                "name": "StandaloneOrbital",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string", "required": true }, { "name": "text", "type": "string" }] },
                "traits": [], "pages": []
            }
        ]
    })
}

fn derive(schema: &Value, orbital: &str) -> DerivedExpectations {
    derive_expectations(schema, orbital, None).expect("derivation")
}

fn expects(d: &DerivedExpectations) -> Value {
    serde_json::to_value(&d.expectations).unwrap()
}

fn diagnostics(d: &DerivedExpectations) -> Value {
    serde_json::to_value(&d.diagnostics).unwrap()
}

fn find<'a>(v: &'a Value, kind: &str, name: Option<&str>) -> Option<&'a Value> {
    v.as_array()?
        .iter()
        .find(|e| e["kind"] == kind && name.is_none_or(|n| e["name"] == n))
}

fn orbital_mut<'a>(schema: &'a mut Value, name: &str) -> &'a mut Value {
    schema["orbitals"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|o| o["name"] == name)
        .unwrap()
}

#[test]
fn derives_an_identity_expectation_from_a_policy_user_role_read_field_copied_verbatim() {
    let d = derive(&build_schema(), "ProductOrbital");
    assert_eq!(diagnostics(&d), json!([]));
    let e = expects(&d);
    let identity = find(&e, "identity", None).expect("identity");
    assert_eq!(identity["name"], "Customer");
    assert_eq!(
        identity["shape"],
        json!([{ "name": "role", "type": "enum", "values": ["customer", "store-manager"], "default": "customer" }])
    );
}

#[test]
fn folds_a_relation_ref_targeting_the_identity_entity_into_the_identity_expectation() {
    let e = expects(&derive(&build_schema(), "ProductOrbital"));
    assert!(find(&e, "entity", Some("Customer")).is_none(), "{e}");
    assert_eq!(find(&e, "identity", None).unwrap()["name"], "Customer");
}

#[test]
fn copies_persist_payload_and_fetch_include_field_types_from_the_provider_and_reads_user_in_guards()
{
    let d = derive(&build_schema(), "CheckoutOrbital");
    assert_eq!(diagnostics(&d), json!([]));
    let e = expects(&d);
    let identity = find(&e, "identity", None).unwrap();
    assert_eq!(identity["name"], "Customer");
    assert_eq!(
        identity["shape"],
        json!([{ "name": "id", "type": "string", "required": true }])
    );
    let order = find(&e, "entity", Some("OrderRecord")).unwrap();
    assert_eq!(
        order["shape"],
        json!([
            { "name": "customer", "type": "relation", "relation": { "entity": "Customer", "cardinality": "one" } },
            { "name": "status", "type": "enum", "values": ["placed", "shipped"] },
            { "name": "totalAmount", "type": "number" }
        ])
    );
}

#[test]
fn never_emits_an_expectation_for_the_orbitals_own_entity() {
    let e = expects(&derive(&build_schema(), "CheckoutOrbital"));
    assert!(find(&e, "entity", Some("Checkout")).is_none());
}

#[test]
fn emits_nothing_for_an_orbital_with_no_cross_boundary_usage() {
    let d = derive(&build_schema(), "StandaloneOrbital");
    assert_eq!(expects(&d), json!([]));
    assert_eq!(diagnostics(&d), json!([]));
}

#[test]
fn a_relation_to_the_identity_entity_merges_into_the_identity_expectation() {
    let e = expects(&derive(&build_schema(), "OrderRecordOrbital"));
    let kinds: Vec<&str> = e
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["identity"]);
}

#[test]
fn reports_a_diagnostic_and_omits_the_shape_entry_when_the_provider_does_not_declare_the_field() {
    let mut schema = build_schema();
    let effects = &mut orbital_mut(&mut schema, "CheckoutOrbital")["traits"][0]["stateMachine"]
        ["transitions"][0]["effects"];
    effects
        .as_array_mut()
        .unwrap()
        .push(json!(["persist", "create", "OrderRecord", { "bogusField": 1 }]));
    effects
        .as_array_mut()
        .unwrap()
        .push(json!(["set", "@entity.customer", "@user.avatar"]));
    let d = derive(&schema, "CheckoutOrbital");
    let e = expects(&d);
    let order_fields: Vec<&str> = find(&e, "entity", Some("OrderRecord")).unwrap()["shape"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(!order_fields.contains(&"bogusField"));
    let identity_fields: Vec<&str> = find(&e, "identity", None).unwrap()["shape"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(!identity_fields.contains(&"avatar"));
    assert_eq!(
        diagnostics(&d),
        json!([
            { "kind": "identity-field-not-declared", "orbital": "CheckoutOrbital", "entity": "Customer", "field": "avatar" },
            { "kind": "entity-field-not-declared", "orbital": "CheckoutOrbital", "entity": "OrderRecord", "field": "bogusField" }
        ])
    );
}

#[test]
fn derives_an_existence_only_entity_expectation_from_a_cross_orbital_linked_entity() {
    let mut schema = build_schema();
    orbital_mut(&mut schema, "StandaloneOrbital")["traits"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "NoteOnCustomer", "scope": "instance", "linkedEntity": "Customer" }));
    assert_eq!(
        expects(&derive(&schema, "StandaloneOrbital")),
        json!([{ "kind": "entity", "name": "Customer" }])
    );
}

#[test]
fn returns_an_unknown_orbital_diagnostic_for_a_name_not_in_the_schema() {
    let d = derive(&build_schema(), "NoSuchOrbital");
    assert_eq!(expects(&d), json!([]));
    assert_eq!(
        diagnostics(&d),
        json!([{ "kind": "unknown-orbital", "orbital": "NoSuchOrbital" }])
    );
}

#[test]
fn derives_an_identity_expectation_from_a_user_config_default_on_a_trait() {
    let mut schema = build_schema();
    orbital_mut(&mut schema, "ProductOrbital")["traits"].as_array_mut().unwrap().push(json!({
        "name": "ProductAppLayout",
        "scope": "instance",
        "linkedEntity": "Product",
        "config": { "viewerName": { "type": "string", "default": "@user.name" }, "appName": { "type": "string", "default": "Shop" } }
    }));
    let d = derive(&schema, "ProductOrbital");
    assert_eq!(diagnostics(&d), json!([]));
    let e = expects(&d);
    let fields: Vec<&str> = find(&e, "identity", None).unwrap()["shape"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(fields, vec!["name", "role"]);
}

#[test]
fn does_not_mutate_the_schema() {
    let schema = build_schema();
    let before = schema.clone();
    let _ = derive(&schema, "ProductOrbital");
    assert_eq!(schema, before);
}

fn schema_with_routes(nav_target: Value) -> Value {
    let mut schema = build_schema();
    orbital_mut(&mut schema, "OrderRecordOrbital")["pages"] =
        json!([{ "name": "OrderDetailPage", "path": "/orders/:id", "traits": [] }]);
    let checkout = orbital_mut(&mut schema, "CheckoutOrbital");
    checkout["pages"] = json!([{ "name": "CheckoutPage", "path": "/checkout", "traits": [] }]);
    checkout["traits"][0]["stateMachine"]["transitions"][0]["effects"] =
        json!([["navigate", nav_target]]);
    schema
}

fn page_paths(schema: &Value, orbital: &str) -> Vec<String> {
    let e = expects(&derive(schema, orbital));
    e.as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "page")
        .map(|x| x["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn resolves_a_dynamic_navigates_static_prefix_to_the_siblings_route_pattern() {
    let schema = schema_with_routes(json!(["str/concat", "/orders/", "@payload.data.id"]));
    assert_eq!(page_paths(&schema, "CheckoutOrbital"), vec!["/orders/:id"]);
}

#[test]
fn prefers_the_parameterised_route_over_its_collection_page() {
    let mut schema = schema_with_routes(json!(["str/concat", "/orders/", "@payload.data.id"]));
    orbital_mut(&mut schema, "OrderRecordOrbital")["pages"] = json!([
        { "name": "OrdersPage", "path": "/orders", "traits": [] },
        { "name": "OrderDetailPage", "path": "/orders/:id", "traits": [] }
    ]);
    assert_eq!(page_paths(&schema, "CheckoutOrbital"), vec!["/orders/:id"]);
}

#[test]
fn derives_nothing_for_a_route_the_organism_does_not_declare() {
    let schema = schema_with_routes(json!(["str/concat", "/ordrs/", "@payload.data.id"]));
    assert!(page_paths(&schema, "CheckoutOrbital").is_empty());
}

#[test]
fn derives_nothing_for_a_route_the_orbital_declares_itself() {
    let schema = schema_with_routes(json!("/checkout"));
    assert!(page_paths(&schema, "CheckoutOrbital").is_empty());
}

#[test]
fn derives_nothing_for_a_fully_dynamic_target() {
    let schema = schema_with_routes(json!("@payload.href"));
    assert!(page_paths(&schema, "CheckoutOrbital").is_empty());
}

fn mod_queue_behavior() -> Value {
    json!({
        "name": "std-mod-queue",
        "orbitals": [{
            "name": "ModQueueItemOrbital",
            "entity": { "name": "ModQueueItem", "fields": [{ "name": "id", "type": "string", "required": true }] },
            "auxiliaryEntities": [{ "name": "ModQueueAudit", "fields": [{ "name": "id", "type": "string", "required": true }] }],
            "traits": [{
                "name": "ModQueueItemReview",
                "scope": "instance",
                "linkedEntity": "ModQueueItem",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "events": [{ "key": "OPEN", "name": "Open" }], "transitions": [{ "from": "idle", "to": "idle", "event": "OPEN" }] }
            }],
            "pages": []
        }]
    })
}

fn load_mod_queue(name: &str) -> Option<Value> {
    (name == "std/behaviors/std-mod-queue").then(mod_queue_behavior)
}

fn schema_composing(rebind_to: Option<&str>) -> Value {
    let mut trait_ref =
        json!({ "ref": "ModQueue.traits.ModQueueItemReview", "name": "ThreadModQueue" });
    if let Some(target) = rebind_to {
        trait_ref["linkedEntity"] = json!(target);
    }
    json!({
        "name": "fixture-forum",
        "orbitals": [
            {
                "name": "WriterOrbital",
                "entity": { "name": "Thread", "fields": [{ "name": "id", "type": "string", "required": true }] },
                "traits": [{
                    "name": "FlagBridge",
                    "scope": "instance",
                    "linkedEntity": "Thread",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "FLAG", "name": "Flag" }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "FLAG", "effects": [["persist", "create", "ModQueueItem", { "status": "pending" }]] }]
                    }
                }],
                "pages": []
            },
            {
                "name": "HostOrbital",
                "entity": { "name": "Decision", "fields": [{ "name": "id", "type": "string", "required": true }] },
                "uses": [{ "as": "ModQueue", "from": "std/behaviors/std-mod-queue" }],
                "traits": [trait_ref],
                "pages": []
            }
        ]
    })
}

fn entity_names(
    schema: &Value,
    orbital: &str,
    load: Option<orbital_core::expects::BehaviorLoader<'_>>,
) -> Vec<String> {
    let d = derive_expectations(schema, orbital, load).unwrap();
    let mut names: Vec<String> = expects(&d)
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "entity")
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn derives_an_expectation_for_an_entity_a_bare_composed_atom_contributes() {
    assert!(entity_names(
        &schema_composing(None),
        "WriterOrbital",
        Some(&load_mod_queue)
    )
    .contains(&"ModQueueItem".to_string()));
}

#[test]
fn does_not_contribute_the_bound_entity_when_the_call_site_rebinds_linked_entity() {
    assert!(!entity_names(
        &schema_composing(Some("Decision")),
        "WriterOrbital",
        Some(&load_mod_queue)
    )
    .contains(&"ModQueueItem".to_string()));
}

#[test]
fn carries_the_imported_orbitals_own_auxiliary_entities_whether_or_not_the_call_site_rebinds() {
    let fetching_aux = |rebind: Option<&str>| {
        let mut schema = schema_composing(rebind);
        schema["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][0]["effects"] =
            json!([["fetch", "ModQueueAudit"]]);
        schema
    };
    assert!(
        entity_names(&fetching_aux(None), "WriterOrbital", Some(&load_mod_queue))
            .contains(&"ModQueueAudit".to_string())
    );
    assert!(entity_names(
        &fetching_aux(Some("Decision")),
        "WriterOrbital",
        Some(&load_mod_queue)
    )
    .contains(&"ModQueueAudit".to_string()));
}

#[test]
fn is_inert_without_a_loader() {
    assert!(
        !entity_names(&schema_composing(None), "WriterOrbital", None)
            .contains(&"ModQueueItem".to_string())
    );
}

#[test]
fn emits_the_contributed_expectation_existence_only_with_no_shape_and_no_field_diagnostics() {
    let d = derive_expectations(
        &schema_composing(None),
        "WriterOrbital",
        Some(&load_mod_queue),
    )
    .unwrap();
    let e = expects(&d);
    let decl = find(&e, "entity", Some("ModQueueItem")).expect("declared");
    assert!(decl.get("shape").is_none(), "{decl}");
    assert!(diagnostics(&d)
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["entity"] != "ModQueueItem"));
}

#[test]
fn never_lets_a_contributed_name_outrank_a_real_declaration() {
    let mut schema = schema_composing(None);
    schema["orbitals"][0]["auxiliaryEntities"] = json!([{ "name": "ModQueueItem", "fields": [{ "name": "id", "type": "string", "required": true }] }]);
    assert!(
        !entity_names(&schema, "WriterOrbital", Some(&load_mod_queue))
            .contains(&"ModQueueItem".to_string())
    );
}

#[test]
fn leaves_a_name_nothing_declares_and_no_atom_contributes_underivable() {
    let mut schema = schema_composing(None);
    schema["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][0]["effects"] =
        json!([["persist", "create", "ModQueueItm", { "status": "pending" }]]);
    assert!(
        !entity_names(&schema, "WriterOrbital", Some(&load_mod_queue))
            .contains(&"ModQueueItm".to_string())
    );
}

fn crm(target: &str) -> Value {
    json!({
        "name": "fixture-crm",
        "orbitals": [
            {
                "name": "TeamMemberOrbital",
                "entity": { "name": "TeamMember", "identity": true, "fields": [{ "name": "id", "type": "string", "required": true }] },
                "auxiliaryEntities": [{ "name": "CrmAssistantChat", "fields": [{ "name": "id", "type": "string", "required": true }] }],
                "traits": [], "pages": []
            },
            {
                "name": "CrmAssistantOrbital",
                "entity": "Agent.orbitals.AgentAssistantOrbital.entity",
                "uses": [{ "as": "Agent", "from": "std/behaviors/std-agent-assistant" }],
                "reference": { "ref": "Agent.orbitals.AgentAssistantOrbital", "entities": { "AgentChat": target } },
                "traits": [], "pages": []
            }
        ]
    })
}

#[test]
fn an_import_remap_target_a_sibling_declares_becomes_expects_entity() {
    assert_eq!(
        expects(&derive(&crm("CrmAssistantChat"), "CrmAssistantOrbital")),
        json!([{ "kind": "entity", "name": "CrmAssistantChat" }])
    );
}

#[test]
fn control_an_import_remap_target_nobody_declares_is_not_invented() {
    assert_eq!(
        expects(&derive(&crm("NoSuchEntity"), "CrmAssistantOrbital")),
        json!([])
    );
}

#[test]
fn control_the_provider_orbital_expects_nothing_for_its_own_entity() {
    let e = expects(&derive(&crm("CrmAssistantChat"), "TeamMemberOrbital"));
    assert!(e.as_array().unwrap().iter().all(|x| x["kind"] != "entity"));
}

fn person_fields() -> Value {
    json!([
        { "name": "id", "type": "string", "required": true },
        { "name": "name", "type": "string", "required": true },
        { "name": "role", "type": "enum", "values": ["member", "staff"], "default": "member" }
    ])
}

fn identity_atom(identity: bool) -> Value {
    let mut entity = json!({ "name": "Person", "fields": person_fields() });
    if identity {
        entity["identity"] = json!(true);
    }
    json!({ "name": "std-identity", "orbitals": [{ "name": "IdentityOrbital", "entity": entity, "traits": [], "pages": [] }] })
}

fn wrapper_atom() -> Value {
    json!({
        "name": "std-account-wrapper",
        "orbitals": [{
            "name": "WrappedIdentityOrbital",
            "entity": "Inner.orbitals.IdentityOrbital.entity",
            "uses": [{ "as": "Inner", "from": "almadar-behaviors/std-identity" }],
            "reference": { "ref": "Inner.orbitals.IdentityOrbital" },
            "traits": [], "pages": []
        }]
    })
}

fn identity_app(reference: &str, from: &str, rename: Option<&str>) -> Value {
    let parts: Vec<&str> = reference.split('.').collect();
    let mut r = json!({ "ref": reference });
    if let Some(name) = rename {
        r["entity"] = json!(name);
    }
    json!({
        "name": "shop",
        "orbitals": [
            {
                "name": "NotesPage",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string", "required": true }] },
                "traits": [{
                    "name": "Greeter",
                    "scope": "instance",
                    "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "INIT", "name": "Init" }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "typography", "content": "@user.name" }]] }]
                    }
                }],
                "pages": []
            },
            {
                "name": "AccountPage",
                "entity": format!("{}.orbitals.{}.entity", parts[0], parts[2]),
                "uses": [{ "as": parts[0], "from": from }],
                "reference": r,
                "traits": [], "pages": []
            }
        ]
    })
}

fn identity_of(schema: &Value, identity: bool) -> Value {
    let load = move |name: &str| match name {
        "almadar-behaviors/std-identity" => Some(identity_atom(identity)),
        "almadar-behaviors/std-account-wrapper" => Some(wrapper_atom()),
        _ => None,
    };
    let e = expects(&derive_expectations(schema, "NotesPage", Some(&load)).unwrap());
    Value::Array(
        e.as_array()
            .unwrap()
            .iter()
            .filter(|x| x["kind"] == "identity")
            .cloned()
            .collect(),
    )
}

#[test]
fn names_the_identity_after_the_imports_local_entity_rename() {
    let app = identity_app(
        "Identity.orbitals.IdentityOrbital",
        "almadar-behaviors/std-identity",
        Some("Customer"),
    );
    assert_eq!(
        identity_of(&app, true),
        json!([{ "kind": "identity", "name": "Customer", "shape": [person_fields()[1]] }])
    );
}

#[test]
fn keeps_the_upstream_name_when_the_import_does_not_rename_the_entity() {
    let app = identity_app(
        "Identity.orbitals.IdentityOrbital",
        "almadar-behaviors/std-identity",
        None,
    );
    assert_eq!(
        identity_of(&app, true),
        json!([{ "kind": "identity", "name": "Person", "shape": [person_fields()[1]] }])
    );
}

#[test]
fn follows_a_nested_import_down_to_the_roster() {
    let app = identity_app(
        "Wrap.orbitals.WrappedIdentityOrbital",
        "almadar-behaviors/std-account-wrapper",
        Some("Customer"),
    );
    assert_eq!(
        identity_of(&app, true),
        json!([{ "kind": "identity", "name": "Customer", "shape": [person_fields()[1]] }])
    );
}

#[test]
fn control_an_imported_entity_that_is_not_identity_leaves_the_expectation_bare() {
    let app = identity_app(
        "Identity.orbitals.IdentityOrbital",
        "almadar-behaviors/std-identity",
        Some("Customer"),
    );
    assert_eq!(identity_of(&app, false), json!([{ "kind": "identity" }]));
}

#[test]
fn control_without_a_loader_the_identity_expectation_stays_bare() {
    let app = identity_app(
        "Identity.orbitals.IdentityOrbital",
        "almadar-behaviors/std-identity",
        Some("Customer"),
    );
    let e = expects(&derive(&app, "NotesPage"));
    let identity: Vec<&Value> = e
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "identity")
        .collect();
    assert_eq!(identity, vec![&json!({ "kind": "identity" })]);
}

#[test]
fn retains_an_authored_identity_contract_without_inventing_a_roster() {
    let authored = json!([{ "kind": "identity", "name": "StudioUser", "shape": [
        { "name": "id", "type": "string", "required": true },
        { "name": "role", "type": "string", "values": ["member", "admin"] }
    ] }]);
    let schema = json!({
        "name": "std-member",
        "orbitals": [{
            "name": "MemberOrbital",
            "entity": {
                "name": "Member",
                "fields": [{ "name": "studioAccount", "type": "relation", "relation": { "entity": "StudioUser", "cardinality": "one" } }],
                "read_policy": ["or", ["=", "@user.role", "admin"], ["=", ["object/get", "@entity", "studioAccount"], "@user.id"]]
            },
            "expects": authored,
            "traits": [], "pages": []
        }]
    });
    let d = derive(&schema, "MemberOrbital");
    assert_eq!(expects(&d), authored);
    assert_eq!(diagnostics(&d), json!([]));
}

#[test]
fn merges_derived_fields_while_keeping_authored_contracts_and_unrelated_dependencies() {
    let mut schema = build_schema();
    let authored = json!([
        { "kind": "identity", "name": "Customer", "shape": [{ "name": "id", "type": "string", "required": true }, { "name": "role", "type": "enum", "values": ["customer"] }] },
        { "kind": "entity", "name": "OrderRecord", "shape": [{ "name": "memo", "type": "string" }] },
        { "kind": "page", "path": "/external" },
        { "kind": "event", "traitName": "Inventory", "event": "READY" },
        { "kind": "event", "traitName": "Inventory", "event": "UPDATED" }
    ]);
    orbital_mut(&mut schema, "CheckoutOrbital")["expects"] = authored.clone();
    let before = schema.clone();
    let e = expects(&derive(&schema, "CheckoutOrbital"));
    let identities: Vec<&Value> = e
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "identity")
        .collect();
    assert_eq!(identities, vec![&authored[0]]);
    let orders: Vec<&Value> = e
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "entity" && x["name"] == "OrderRecord")
        .collect();
    assert_eq!(orders.len(), 1);
    let shape = orders[0]["shape"].as_array().unwrap();
    assert!(
        shape.contains(&json!({ "name": "memo", "type": "string" }))
            && shape.contains(&json!({ "name": "totalAmount", "type": "number" })),
        "{shape:?}"
    );
    for kept in [&authored[2], &authored[3], &authored[4]] {
        assert!(e.as_array().unwrap().contains(kept), "{kept} kept");
    }
    assert_eq!(schema, before);
}

#[test]
fn preserves_authored_entity_and_identity_contracts_for_the_same_provider() {
    let mut schema = build_schema();
    let authored = json!([
        { "kind": "entity", "name": "Customer", "shape": [{ "name": "email", "type": "email" }] },
        { "kind": "identity", "name": "Customer", "shape": [{ "name": "role", "type": "string", "values": ["customer"] }] }
    ]);
    orbital_mut(&mut schema, "CheckoutOrbital")["expects"] = authored.clone();
    let e = expects(&derive(&schema, "CheckoutOrbital"));
    let identities: Vec<&Value> = e
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "identity")
        .collect();
    assert_eq!(identities.len(), 1);
    assert!(e.as_array().unwrap().contains(&authored[0]));
    assert_eq!(identities[0]["name"], "Customer");
    let shape = identities[0]["shape"].as_array().unwrap();
    assert!(
        shape.contains(&json!({ "name": "role", "type": "string", "values": ["customer"] }))
            && shape.contains(&json!({ "name": "id", "type": "string", "required": true })),
        "{shape:?}"
    );
}

#[test]
fn preserves_a_bare_identity_requirement_with_no_inferred_fields() {
    let schema = json!({ "name": "bare", "orbitals": [{ "name": "Bare", "entity": { "name": "Item", "fields": [] }, "traits": [], "pages": [], "expects": [{ "kind": "identity" }] }] });
    let d = derive(&schema, "Bare");
    assert_eq!(expects(&d), json!([{ "kind": "identity" }]));
    assert_eq!(diagnostics(&d), json!([]));
}

// The event arm (new with the port): a listen whose source is a sibling orbital's trait.
fn schema_with_listen(source: Value) -> Value {
    let mut schema = build_schema();
    orbital_mut(&mut schema, "OrderRecordOrbital")["traits"] = json!([{
        "name": "OrderFeed",
        "scope": "instance",
        "linkedEntity": "OrderRecord",
        "emits": [{ "event": "ORDER_PLACED" }],
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "events": [{ "key": "INIT", "name": "Init" }], "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }] }
    }]);
    orbital_mut(&mut schema, "StandaloneOrbital")["traits"] = json!([{
        "name": "NoteFeed",
        "scope": "instance",
        "linkedEntity": "Note",
        "listens": [{ "event": "ORDER_PLACED", "source": source, "triggers": "INIT" }],
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "events": [{ "key": "INIT", "name": "Init" }], "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }] }
    }]);
    schema
}

#[test]
fn a_listen_on_a_sibling_orbitals_trait_derives_expects_event() {
    let schema = schema_with_listen(
        json!({ "kind": "orbital", "orbital": "OrderRecordOrbital", "trait": "OrderFeed" }),
    );
    let d = derive(&schema, "StandaloneOrbital");
    assert_eq!(
        expects(&d),
        json!([{ "kind": "event", "traitName": "OrderFeed", "event": "ORDER_PLACED" }])
    );
    assert_eq!(diagnostics(&d), json!([]));
}

#[test]
fn a_listen_on_the_same_orbitals_trait_derives_nothing() {
    let schema = schema_with_listen(json!({ "kind": "trait", "trait": "NoteFeed" }));
    assert_eq!(expects(&derive(&schema, "StandaloneOrbital")), json!([]));
}

#[test]
fn a_listen_on_an_orbital_the_program_lacks_is_a_diagnostic_not_an_expectation() {
    let schema = schema_with_listen(
        json!({ "kind": "orbital", "orbital": "NoSuchOrbital", "trait": "OrderFeed" }),
    );
    let d = derive(&schema, "StandaloneOrbital");
    assert_eq!(expects(&d), json!([]));
    assert_eq!(
        diagnostics(&d),
        json!([{ "kind": "event-source-unknown", "orbital": "StandaloneOrbital", "field": "NoSuchOrbital.OrderFeed.ORDER_PLACED" }])
    );
}

#[test]
fn a_call_site_listen_on_a_composed_trait_derives_expects_event_too() {
    let mut schema = schema_with_listen(json!({ "kind": "trait", "trait": "NoteFeed" }));
    orbital_mut(&mut schema, "StandaloneOrbital")["traits"] = json!([{
        "ref": "Feed.traits.Feed",
        "name": "NoteFeed",
        "listens": [{ "event": "ORDER_PLACED", "source": { "kind": "orbital", "orbital": "OrderRecordOrbital", "trait": "OrderFeed" }, "triggers": "INIT" }]
    }]);
    assert_eq!(
        expects(&derive(&schema, "StandaloneOrbital")),
        json!([{ "kind": "event", "traitName": "OrderFeed", "event": "ORDER_PLACED" }])
    );
}

// Shape union, the derivation's merge (twin of `@almadar/core` `mergeExpectationShape`, which the JS composer keeps).
#[test]
fn shape_union_retains_an_explicit_relation_target_identity() {
    let prior = json!({ "name": "owner", "type": "relation", "relation": { "entity": "Member", "cardinality": "one" } });
    let incoming = json!({ "name": "owner", "type": "relation", "relation": { "entity": "Member", "cardinality": "one", "entityId": "ent_01M4CP7RDD5PE3TMDYAGDG28BD" } });
    assert_eq!(
        merge_expectation_shape(Some(&json!([prior])), Some(&json!([incoming.clone()]))).unwrap(),
        Some(json!([incoming]))
    );
}

#[test]
fn shape_union_rejects_conflicting_relation_target_identities() {
    let prior = json!({ "name": "owner", "type": "relation", "relation": { "entity": "Member", "cardinality": "one", "entityId": "ent_01M4CP7RDD5PE3TMDYAGDG28BD" } });
    let incoming = json!({ "name": "owner", "type": "relation", "relation": { "entity": "Member", "cardinality": "one", "entityId": "ent_01M4CPARK4JPKGG21HZN7ZQDHH" } });
    let err = merge_expectation_shape(Some(&json!([prior])), Some(&json!([incoming]))).unwrap_err();
    assert!(err.contains("expectation shape mismatch"), "{err}");
}

#[test]
fn shape_union_merges_recursive_properties_array_items_requiredness_vocabulary_and_ranges() {
    let prior = json!([
        { "name": "profile", "type": "object", "properties": { "role": { "type": "string", "values": ["member"], "description": "keep" }, "count": { "type": "number", "min": 1, "max": 10 } } },
        { "name": "rows", "type": "array", "items": { "type": "object", "properties": { "label": { "type": "string" } } } }
    ]);
    let incoming = json!([
        { "name": "profile", "type": "object", "properties": { "count": { "type": "number", "min": 3, "max": 8 }, "role": { "type": "enum", "values": ["admin"], "required": true, "description": "other" }, "email": { "type": "email" } } },
        { "name": "rows", "type": "array", "items": { "type": "object", "properties": { "count": { "type": "number" } } } }
    ]);
    let (p0, i0) = (prior.clone(), incoming.clone());
    assert_eq!(
        merge_expectation_shape(Some(&prior), Some(&incoming)).unwrap(),
        Some(json!([
            { "name": "profile", "type": "object", "properties": { "role": { "type": "string", "values": ["member", "admin"], "required": true, "description": "keep" }, "count": { "type": "number", "min": 3, "max": 8 }, "email": { "type": "email" } } },
            { "name": "rows", "type": "array", "items": { "type": "object", "properties": { "label": { "type": "string" }, "count": { "type": "number" } } } }
        ]))
    );
    assert_eq!((prior, incoming), (p0, i0));
}

#[test]
fn shape_union_accepts_reordered_structured_keys_and_same_arity_tuples() {
    let prior = json!({ "name": "pair", "type": "tuple", "properties": { "0": { "type": "string", "default": "first" }, "1": { "type": "object", "properties": { "a": { "type": "boolean" }, "b": { "type": "number" } } } } });
    let incoming = json!({ "name": "pair", "type": "tuple", "properties": { "1": { "type": "object", "properties": { "b": { "type": "number" }, "a": { "type": "boolean" } } }, "0": { "type": "string", "default": "other", "required": true } } });
    let merged = merge_expectation_shape(Some(&json!([prior])), Some(&json!([incoming])))
        .unwrap()
        .unwrap();
    assert_eq!(merged[0]["properties"]["0"]["default"], "first");
    assert_eq!(merged[0]["properties"]["0"]["required"], true);
}

#[test]
fn shape_union_refuses_incompatible_constraints_instead_of_dropping_them() {
    for (prior, incoming) in [
        (
            json!({ "name": "field", "type": "string" }),
            json!({ "name": "field", "type": "number" }),
        ),
        (
            json!({ "name": "field", "type": "relation", "relation": { "entity": "A", "cardinality": "one" } }),
            json!({ "name": "field", "type": "relation", "relation": { "entity": "B", "cardinality": "one" } }),
        ),
        (
            json!({ "name": "field", "type": "array", "items": { "type": "string" } }),
            json!({ "name": "field", "type": "array", "items": { "type": "number" } }),
        ),
        (
            json!({ "name": "field", "type": "tuple", "properties": { "0": { "type": "string" } } }),
            json!({ "name": "field", "type": "tuple", "properties": { "0": { "type": "string" }, "1": { "type": "string" } } }),
        ),
        (
            json!({ "name": "field", "type": "number", "min": 5 }),
            json!({ "name": "field", "type": "number", "max": 3 }),
        ),
    ] {
        let err = merge_expectation_shape(
            Some(&json!([prior.clone()])),
            Some(&json!([incoming.clone()])),
        )
        .unwrap_err();
        assert!(
            err.contains("expectation shape mismatch"),
            "{prior} + {incoming}: {err}"
        );
    }
}

fn with_inherited_identity(name: &str) -> Value {
    let mut schema = build_schema();
    orbital_mut(&mut schema, "CheckoutOrbital")["expects"] = json!([{
        "kind": "identity",
        "name": name,
        "shape": [{ "name": "role", "type": "enum", "values": ["customer", "store-manager"] }]
    }]);
    schema
}

#[test]
fn an_inherited_identity_naming_another_roster_is_the_programs_identity() {
    let e = expects(&derive(
        &with_inherited_identity("StudioUser"),
        "CheckoutOrbital",
    ));
    let identities: Vec<&Value> = e
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "identity")
        .collect();
    assert_eq!(identities.len(), 1, "{e}");
    assert_eq!(identities[0]["name"], "Customer", "{e}");
    assert!(
        identities[0]["shape"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "role"),
        "the inherited shape is kept: {e}"
    );
}

#[test]
fn control_an_inherited_identity_naming_the_bound_roster_stays_one_entry() {
    let e = expects(&derive(
        &with_inherited_identity("Customer"),
        "CheckoutOrbital",
    ));
    let n = e
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "identity")
        .count();
    assert_eq!(n, 1, "{e}");
}

#[test]
fn edge_without_a_bound_identity_the_inherited_name_stays() {
    let mut schema = with_inherited_identity("StudioUser");
    orbital_mut(&mut schema, "CustomerOrbital")["entity"]["identity"] = json!(false);
    let e = expects(&derive(&schema, "CheckoutOrbital"));
    assert!(
        e.as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "identity" && x["name"] == "StudioUser"),
        "{e}"
    );
}
