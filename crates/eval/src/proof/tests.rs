use super::*;
use orbital_core_base::schema::types::{EntityRef, TraitReference};
use serde_json::{json, Value as J};

/// One orbital: entity `Order`, trait `Checkout -> Order` with the given transitions/ticks, and the given strict
/// spec expectations already merged onto it.
fn program(transitions: J, ticks: J, expects: J) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Shop",
            "entity": { "name": "Order", "collection": "orders", "persistence": "persistent", "fields": [
                { "name": "id", "type": "string", "required": true },
                { "name": "total", "type": "number", "required": true },
                { "name": "paid", "type": "number" },
                { "name": "note", "type": "string", "intrinsic": true },
                { "name": "status", "type": "enum", "values": ["open", "paid", "refunded"], "default": "open" }
            ] },
            "expects": expects,
            "traits": [{
                "name": "Checkout",
                "linkedEntity": "Order",
                "stateMachine": {
                    "states": [{ "name": "open", "isInitial": true }, { "name": "paid" }],
                    "events": [
                        { "key": "INIT", "name": "Init" },
                        { "key": "PAY", "name": "Pay", "payloadSchema": [
                            { "name": "amount", "type": "number", "required": true },
                            { "name": "status", "type": "string" },
                            { "name": "tip", "type": "number" }
                        ] },
                        { "key": "ADD", "name": "Add", "payloadSchema": [{ "name": "total", "type": "number", "required": true }] }
                    ],
                    "transitions": transitions
                },
                "ticks": ticks
            }],
            "pages": []
        }]
    }))
    .expect("fixture deserializes")
}

fn row_invariant(expr: J) -> J {
    json!([{ "kind": "entity", "name": "Order", "invariants": [expr], "strict": true }])
}

fn state_holds(state: &str, expr: J) -> J {
    json!([{ "kind": "state", "traitName": "Checkout", "state": state, "holds": expr, "strict": true }])
}

fn verdicts(schema: &OrbitalSchema) -> Vec<(String, Verdict)> {
    prove(schema)
        .into_iter()
        .map(|f| (f.site, f.verdict))
        .collect()
}

fn only(schema: &OrbitalSchema) -> Verdict {
    let v = verdicts(schema);
    assert_eq!(v.len(), 1, "{v:#?}");
    v.into_iter().next().unwrap().1
}

#[test]
fn no_propositions_owe_nothing() {
    let t = json!([{ "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "create", "Order", { "total": -1 }]] }]);
    assert!(prove(&program(t, J::Null, J::Null)).is_empty());
}

#[test]
fn record_contract_required_nullable_payload_does_not_prove_a_nonnull_invariant() {
    let transition = json!([{ "from": "open", "to": "open", "event": "ADD",
        "effects": [["persist", "create", "Order", { "total": "@payload.total" }]] }]);
    let invariant = row_invariant(json!(["!=", "@entity.total", null]));
    for (required, nullable, proved) in [
        (false, false, false),
        (true, false, true),
        (false, true, false),
        (true, true, false),
    ] {
        let mut wire =
            serde_json::to_value(program(transition.clone(), J::Null, invariant.clone()))
                .unwrap();
        wire["typeContractVersion"] = json!(1);
        wire["orbitals"][0]["entity"]["fields"][1]["nullable"] = json!(true);
        wire["orbitals"][0]["traits"][0]["stateMachine"]["events"][2]["payloadSchema"][0]
            ["nullable"] = json!(nullable);
        wire["orbitals"][0]["traits"][0]["stateMachine"]["events"][2]["payloadSchema"][0]
            ["required"] = json!(required);
        let schema: OrbitalSchema = serde_json::from_value(wire).unwrap();
        let verdict = only(&schema);
        if proved {
            assert_eq!(verdict, Verdict::Proved);
        } else if required {
            assert!(
                matches!(verdict, Verdict::Violated { .. }),
                "required nullable payload: {verdict:?}"
            );
        } else {
            assert!(
                !matches!(verdict, Verdict::Proved),
                "optional payload (nullable={nullable}): {verdict:?}"
            );
        }
    }
}

#[test]
fn record_contract_presence_proof_distinguishes_an_absent_field_from_present_null() {
    for (row, expected) in [(json!({"total": null}), true), (json!({}), false)] {
        let transition = json!([{ "from": "open", "to": "open", "event": "ADD",
            "effects": [["persist", "create", "Order", row]] }]);
        let mut wire = serde_json::to_value(program(
            transition,
            J::Null,
            row_invariant(json!(["object/has", "@entity", "total"])),
        ))
        .unwrap();
        wire["typeContractVersion"] = json!(1);
        wire["orbitals"][0]["entity"]["fields"][1]["required"] = json!(false);
        wire["orbitals"][0]["entity"]["fields"][1]["nullable"] = json!(true);
        let schema: OrbitalSchema = serde_json::from_value(wire).unwrap();
        let verdict = only(&schema);
        if expected {
            assert_eq!(verdict, Verdict::Proved, "present-null is present");
        } else {
            assert!(
                matches!(verdict, Verdict::Violated { .. }),
                "absent field: {verdict:?}"
            );
        }
    }
}

#[test]
fn a_create_is_proved_by_its_guard_and_refuted_without_it() {
    let inv = row_invariant(json!([">=", "@entity.total", 0]));
    let guarded = json!([{ "from": "open", "to": "open", "event": "ADD", "guard": [">=", "@payload.total", 0],
        "effects": [["persist", "create", "Order", { "total": "@payload.total" }]] }]);
    assert_eq!(
        only(&program(guarded, J::Null, inv.clone())),
        Verdict::Proved
    );
    let bare = json!([{ "from": "open", "to": "open", "event": "ADD",
        "effects": [["persist", "create", "Order", { "total": "@payload.total" }]] }]);
    let Verdict::Violated { counterexample } = only(&program(bare, J::Null, inv)) else {
        panic!()
    };
    assert!(
        counterexample["payload"]["total"].as_f64().unwrap() < 0.0,
        "{counterexample}"
    );
}

#[test]
fn an_own_row_update_is_proved_from_the_old_row_and_its_guard() {
    let inv = row_invariant(json!(["<=", "@entity.paid", "@entity.total"]));
    let pay = |guard: Option<J>| {
        let mut t = json!({ "from": "open", "to": "paid", "event": "PAY",
            "effects": [["persist", "update", "Order", "@entity.id", { "paid": ["+", "@entity.paid", "@payload.amount"] }]] });
        if let Some(g) = guard {
            t["guard"] = g;
        }
        json!([t])
    };
    let ok = pay(Some(json!([
        "<=",
        ["+", "@entity.paid", "@payload.amount"],
        "@entity.total"
    ])));
    assert_eq!(only(&program(ok, J::Null, inv.clone())), Verdict::Proved);
    let Verdict::Violated { counterexample } = only(&program(pay(None), J::Null, inv)) else {
        panic!()
    };
    let row = &counterexample["row"];
    let paid = row["paid"].as_f64().unwrap_or(0.0)
        + counterexample["payload"]["amount"].as_f64().unwrap();
    assert!(paid > row["total"].as_f64().unwrap(), "{counterexample}");
}

#[test]
fn an_upsert_owes_both_cases_and_delete_owes_nothing() {
    let inv = row_invariant(json!([">=", "@entity.total", 0]));
    let t = json!([
        { "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "upsert", "Order", { "id": "x", "total": 5 }]] },
        { "from": "paid", "to": "paid", "event": "ADD", "effects": [["persist", "delete", "Order", "@entity.id"]] }
    ]);
    let v = verdicts(&program(t, J::Null, inv));
    assert_eq!(v.len(), 2, "{v:#?}");
    assert!(
        v.iter().all(|(_, verdict)| *verdict == Verdict::Proved),
        "{v:#?}"
    );
}

#[test]
fn a_batched_create_is_checked_like_a_single_one() {
    let inv = row_invariant(json!([">=", "@entity.total", 0]));
    let t = json!([{ "from": "open", "to": "open", "event": "ADD",
        "effects": [["persist", "batch", [["create", "Order", { "total": -3 }]]]] }]);
    assert!(matches!(
        only(&program(t, J::Null, inv)),
        Verdict::Violated { .. }
    ));
}

#[test]
fn an_intrinsic_field_is_never_stored() {
    let inv = row_invariant(json!(["=", "@entity.note", null]));
    let t = json!([{ "from": "open", "to": "open", "event": "ADD",
        "effects": [["persist", "create", "Order", { "total": 1, "note": "hi" }]] }]);
    assert_eq!(only(&program(t, J::Null, inv)), Verdict::Proved);
}

#[test]
fn an_undecidable_atom_is_unproved_and_named() {
    let inv = row_invariant(json!([">", ["str/len", "@entity.status"], 0]));
    let t = json!([{ "from": "open", "to": "open", "event": "ADD",
        "effects": [["persist", "create", "Order", { "total": 1, "status": "open" }]] }]);
    let Verdict::Unproved { reason, .. } = only(&program(t, J::Null, inv)) else {
        panic!()
    };
    assert!(reason.contains("str/len"), "{reason}");
}

#[test]
fn a_state_invariant_follows_the_sets_into_the_state() {
    let holds = state_holds("paid", json!(["=", "@entity.status", "paid"]));
    let to_paid = |status: &str| json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.status", status]] }]);
    assert_eq!(
        only(&program(to_paid("paid"), J::Null, holds.clone())),
        Verdict::Proved
    );
    let Verdict::Violated { .. } = only(&program(to_paid("open"), J::Null, holds)) else {
        panic!()
    };
}

#[test]
fn a_transition_out_of_a_state_assumes_that_states_own_proposition() {
    let into = json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.paid", ["+", "@entity.paid", 1]]] }]);
    let both = |source: J| {
        let mut all = state_holds("paid", json!([">=", "@entity.paid", 1]))
            .as_array()
            .unwrap()
            .clone();
        all.extend(source.as_array().unwrap().clone());
        J::Array(all)
    };
    let v = verdicts(&program(
        into.clone(),
        J::Null,
        both(state_holds("open", json!([">=", "@entity.paid", 0]))),
    ));
    let entry = v.iter().find(|(s, _)| s.contains("PAY")).unwrap();
    assert_eq!(entry.1, Verdict::Proved, "{v:#?}");
    let v = verdicts(&program(
        into,
        J::Null,
        state_holds("paid", json!([">=", "@entity.paid", 1])),
    ));
    assert!(
        matches!(
            v.iter().find(|(s, _)| s.contains("PAY")).unwrap().1,
            Verdict::Violated { .. }
        ),
        "{v:#?}"
    );
}

#[test]
fn a_guard_the_prover_cannot_model_never_makes_a_false_goal_proved() {
    let holds = state_holds("paid", json!(["=", "@entity.status", "paid"]));
    let t = json!([{ "from": "open", "to": "paid", "event": "PAY", "guard": ["and", "@config.enabled", [">", ["array/len", "@payload.data"], 0]],
        "effects": [["set", "@entity.status", "open"]] }]);
    assert!(!matches!(
        only(&program(t, J::Null, holds)),
        Verdict::Proved
    ));
}

#[test]
fn max_min_and_nested_ifs_are_proved_only_when_exact() {
    let holds = state_holds("paid", json!([">=", "@entity.paid", 0]));
    let tick = |value: J| json!([{ "name": "drain", "interval": 1000, "appliesTo": ["paid"], "effects": [["set", "@entity.paid", value]] }]);
    let into = json!([{ "from": "open", "to": "paid", "event": "PAY", "guard": [">=", "@payload.amount", 0], "effects": [["set", "@entity.paid", "@payload.amount"]] }]);
    let floored = tick(json!(["math/max", 0, ["-", "@entity.paid", 1]]));
    let v = verdicts(&program(into.clone(), floored, holds.clone()));
    assert!(v.iter().all(|(_, x)| *x == Verdict::Proved), "{v:#?}");
    let unfloored = tick(json!(["-", "@entity.paid", 1]));
    let v = verdicts(&program(into.clone(), unfloored, holds.clone()));
    assert!(
        v.iter()
            .any(|(s, x)| s.contains("tick drain") && matches!(x, Verdict::Violated { .. })),
        "{v:#?}"
    );
    let nullable_operand = tick(json!(["math/max", 0, "@payload.missing"]));
    let v = verdicts(&program(into, nullable_operand, holds));
    assert!(
        v.iter()
            .any(|(s, x)| s.contains("tick drain") && matches!(x, Verdict::Unproved { .. })),
        "{v:#?}"
    );
}

#[test]
fn the_clock_is_a_positive_number() {
    let inv = row_invariant(json!([">", "@entity.paid", 0]));
    let stamp = |value: J| json!([{ "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "create", "Order", { "total": 1, "paid": value }]] }]);
    assert_eq!(
        only(&program(stamp(json!(["time/now"])), J::Null, inv.clone())),
        Verdict::Proved
    );
    assert_eq!(
        only(&program(stamp(json!("@now")), J::Null, inv.clone())),
        Verdict::Proved
    );
    assert!(!matches!(
        only(&program(stamp(json!(["-", ["time/now"], 5])), J::Null, inv)),
        Verdict::Proved
    ));
}

#[test]
fn an_array_length_is_a_non_negative_number() {
    let inv = row_invariant(json!([">=", "@entity.total", 0]));
    let create = |total: J| json!([{ "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "create", "Order", { "total": total }]] }]);
    assert_eq!(
        only(&program(
            create(json!(["array/len", "@payload.rows"])),
            J::Null,
            inv.clone()
        )),
        Verdict::Proved
    );
    assert_eq!(
        only(&program(
            create(json!(["default", ["array/len", "@payload.rows"], 0])),
            J::Null,
            inv.clone()
        )),
        Verdict::Proved
    );
    assert!(!matches!(
        only(&program(
            create(json!(["-", ["array/len", "@payload.rows"], 1])),
            J::Null,
            inv
        )),
        Verdict::Proved
    ));
}

#[test]
fn a_merged_payload_record_is_its_literal_keys_over_the_record() {
    let inv = row_invariant(json!([">=", "@entity.total", 0]));
    let create = |over: J| {
        json!([{ "from": "open", "to": "open", "event": "ADD",
        "effects": [["persist", "create", "Order", ["object/merge", "@payload", over]]] }])
    };
    assert_eq!(
        only(&program(
            create(json!({ "total": 5 })),
            J::Null,
            inv.clone()
        )),
        Verdict::Proved
    );
    assert!(
        matches!(
            only(&program(create(json!({ "paid": 0 })), J::Null, inv)),
            Verdict::Violated { .. }
        ),
        "control: the record's own total is unconstrained"
    );
}

#[test]
fn a_branching_effect_is_split_per_branch() {
    let holds = state_holds("paid", json!(["=", "@entity.status", "paid"]));
    let t = json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [
        ["if", [">", "@payload.amount", 0], ["set", "@entity.status", "paid"], ["set", "@entity.status", "refunded"]]
    ] }]);
    let v = verdicts(&program(t.clone(), J::Null, holds.clone()));
    assert_eq!(v.len(), 2, "{v:#?}");
    assert_eq!(
        v.iter()
            .filter(|(_, x)| matches!(x, Verdict::Violated { .. }))
            .count(),
        1,
        "{v:#?}"
    );
    let guarded = json!([{ "from": "open", "to": "paid", "event": "PAY", "guard": [">", "@payload.amount", 0], "effects": t[0]["effects"] }]);
    let v = verdicts(&program(guarded, J::Null, holds));
    assert!(v.iter().all(|(_, x)| *x == Verdict::Proved), "{v:#?}");
}

#[test]
fn a_tick_in_the_state_must_preserve_it() {
    let holds = state_holds("paid", json!([">=", "@entity.paid", 0]));
    let ticks = |delta: f64| {
        json!([{ "name": "accrue", "interval": 1000, "appliesTo": ["paid"],
        "effects": [["set", "@entity.paid", ["+", "@entity.paid", delta]]] }])
    };
    let into = json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.paid", "@payload.amount"]], "guard": [">=", "@payload.amount", 0] }]);
    let v = verdicts(&program(into.clone(), ticks(1.0), holds.clone()));
    assert!(v.iter().all(|(_, x)| *x == Verdict::Proved), "{v:#?}");
    let v = verdicts(&program(into, ticks(-1.0), holds));
    assert!(
        v.iter()
            .any(|(s, x)| s.contains("tick accrue") && matches!(x, Verdict::Violated { .. })),
        "{v:#?}"
    );
}

#[test]
fn an_initial_state_owes_its_defaults() {
    let holds = |expr: J| state_holds("open", expr);
    let v = verdicts(&program(
        json!([]),
        J::Null,
        holds(json!(["=", "@entity.status", "open"])),
    ));
    let defaults = v.iter().find(|(s, _)| s.contains("defaults")).unwrap();
    assert_eq!(defaults.1, Verdict::Proved);
    let v = verdicts(&program(
        json!([]),
        J::Null,
        holds(json!(["=", "@entity.status", "paid"])),
    ));
    assert!(
        matches!(
            v.iter().find(|(s, _)| s.contains("defaults")).unwrap().1,
            Verdict::Violated { .. }
        ),
        "{v:#?}"
    );
}

// G-ORBITAL-CORE-019: a `set` of a binding that reads nothing stores null, so an optional binding is its value or null.
#[test]
fn a_set_from_an_optional_binding_is_its_value_or_null() {
    let holds = state_holds("paid", json!(["!=", "@entity.status", "refunded"]));
    let optional = json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.status", "@payload.status"]] }]);
    assert!(
        matches!(
            only(&program(optional, J::Null, holds.clone())),
            Verdict::Violated { .. }
        ),
        "the payload may carry `refunded`"
    );
    let required = json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.paid", "@payload.amount"], ["set", "@entity.status", "paid"]] }]);
    assert_eq!(only(&program(required, J::Null, holds)), Verdict::Proved);
}

#[test]
fn an_equality_over_a_set_from_an_optional_binding_is_proved() {
    let holds = state_holds("paid", json!(["=", "@entity.status", "@payload.status"]));
    let optional = json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.status", "@payload.status"]] }]);
    assert_eq!(only(&program(optional, J::Null, holds)), Verdict::Proved);
    let ensures = json!([["=", "@entity.paid", "@payload.tip"]]);
    let t = json!([pay(
        json!([["set", "@entity.paid", "@payload.tip"]]),
        ensures,
        None
    )]);
    assert_eq!(only(&program(t, J::Null, J::Null)), Verdict::Proved);
    let signed = json!([pay(
        json!([["set", "@entity.paid", "@payload.tip"]]),
        json!([[">=", "@entity.paid", 0]]),
        None
    )]);
    assert!(
        matches!(
            only(&program(signed, J::Null, J::Null)),
            Verdict::Violated { .. }
        ),
        "control: an optional tip may be negative"
    );
}

#[test]
fn a_payload_read_in_a_guard_and_a_holds_is_proved_and_its_control_refuted() {
    let holds = state_holds("open", json!(["<", "@entity.paid", "@entity.total"]));
    let raise = |guard: Option<J>| {
        let mut t = json!({ "from": "open", "to": "open", "event": "ADD", "effects": [["set", "@entity.total", "@payload.total"]] });
        if let Some(g) = guard {
            t["guard"] = g;
        }
        json!([t])
    };
    let v = verdicts(&program(
        raise(Some(json!(["<", "@entity.paid", "@payload.total"]))),
        J::Null,
        holds.clone(),
    ));
    assert!(
        v.iter()
            .filter(|(s, _)| s.contains("ADD"))
            .all(|(_, x)| *x == Verdict::Proved),
        "{v:#?}"
    );
    let v = verdicts(&program(raise(None), J::Null, holds));
    assert!(
        v.iter()
            .any(|(s, x)| s.contains("ADD") && matches!(x, Verdict::Violated { .. })),
        "control: without the guard the new total may sit below paid {v:#?}"
    );
}

#[test]
fn an_entity_read_inside_a_set_is_the_pre_state_value() {
    let copy = json!([["set", "@entity.paid", "@entity.total"]]);
    let t = json!([pay(
        copy.clone(),
        json!([["=", "@entity.paid", "@entity.total"]]),
        None
    )]);
    assert_eq!(only(&program(t, J::Null, J::Null)), Verdict::Proved);
    let t = json!([pay(
        copy.clone(),
        json!([["=", "@entity.paid", "@fromEntity.total"]]),
        None
    )]);
    assert_eq!(only(&program(t, J::Null, J::Null)), Verdict::Proved);
    let t = json!([pay(
        copy,
        json!([["=", "@entity.paid", "@fromEntity.paid"]]),
        None
    )]);
    assert!(
        matches!(
            only(&program(t, J::Null, J::Null)),
            Verdict::Violated { .. }
        ),
        "control: the old paid is gone"
    );
    let reset = json!([
        ["set", "@entity.total", 0],
        ["set", "@entity.paid", "@entity.total"]
    ]);
    let t = json!([pay(reset, json!([["=", "@entity.paid", 0]]), None)]);
    assert_eq!(
        only(&program(t, J::Null, J::Null)),
        Verdict::Proved,
        "a later set reads the earlier one"
    );
}

/// The fixture with `Checkout`'s config knobs set to the given fields.
fn configured(transitions: J, knobs: J) -> OrbitalSchema {
    use orbital_core_base::schema::types::ConfigField;
    let mut schema = program(transitions, J::Null, J::Null);
    let TraitReference::Inline(td) = &mut schema.orbitals[0].traits[0] else {
        panic!()
    };
    let knobs: orbital_core_base::collections::IndexMap<String, ConfigField> =
        serde_json::from_value(knobs).unwrap();
    td.config = Some(knobs);
    schema
}

#[test]
fn a_config_knob_is_the_runs_constant() {
    let knobs = json!({ "cap": { "type": "number", "default": 10 }, "on": { "type": "boolean", "default": true } });
    let capped = |effects: J| {
        json!([pay(
            effects,
            json!([["<=", "@entity.paid", "@config.cap"]]),
            None
        )])
    };
    assert_eq!(
        only(&configured(
            capped(json!([["set", "@entity.paid", "@config.cap"]])),
            knobs.clone()
        )),
        Verdict::Proved
    );
    assert!(
        matches!(
            only(&configured(
                capped(json!([["set", "@entity.paid", ["+", "@config.cap", 1]]])),
                knobs.clone()
            )),
            Verdict::Violated { .. }
        ),
        "control: one past the cap"
    );
    let guarded = json!([pay(
        json!([["set", "@entity.paid", ["+", "@entity.paid", 1]]]),
        json!([["<", "@fromEntity.paid", "@config.cap"]]),
        Some(json!(["<", "@entity.paid", "@config.cap"]))
    )]);
    assert_eq!(
        only(&configured(guarded, knobs)),
        Verdict::Proved,
        "a guard reads the knob"
    );
}

#[test]
fn a_count_gated_by_a_config_flag_is_proved_per_flag() {
    let count = json!([[
        "when",
        "@config.on",
        ["set", "@entity.paid", ["+", "@entity.paid", 1]]
    ]]);
    let either = json!([[
        "if",
        "@config.on",
        ["=", "@entity.paid", ["+", "@fromEntity.paid", 1]],
        ["=", "@entity.paid", "@fromEntity.paid"]
    ]]);
    let grows = json!([["=", "@entity.paid", ["+", "@fromEntity.paid", 1]]]);
    let flag = |on: bool| json!({ "on": { "type": "boolean", "default": on } });
    let proved = |s: &OrbitalSchema| verdicts(s).iter().all(|(_, v)| *v == Verdict::Proved);
    for on in [true, false] {
        assert!(
            proved(&configured(
                json!([pay(count.clone(), either.clone(), None)]),
                flag(on)
            )),
            "on = {on}"
        );
    }
    assert!(proved(&configured(
        json!([pay(count.clone(), grows.clone(), None)]),
        flag(true)
    )));
    let v = verdicts(&configured(json!([pay(count, grows, None)]), flag(false)));
    assert!(
        v.iter().any(|(_, x)| matches!(x, Verdict::Violated { .. })),
        "control: a disabled flag counts nothing {v:#?}"
    );
}

#[test]
fn a_config_knob_bound_at_run_time_is_unproved_and_an_unset_one_is_null() {
    let ensures = json!([["=", "@entity.paid", "@config.cap"]]);
    let t = json!([pay(
        json!([["set", "@entity.paid", 3]]),
        ensures.clone(),
        None
    )]);
    let bound = json!({ "cap": { "type": "number", "default": "@user.cap" } });
    assert!(
        matches!(
            only(&configured(t.clone(), bound)),
            Verdict::Unproved { .. }
        ),
        "a viewer-bound knob is never refuted"
    );
    let unset = json!({ "cap": { "type": "number" } });
    assert!(
        matches!(
            only(&configured(t, unset.clone())),
            Verdict::Violated { .. }
        ),
        "an unset knob reads null"
    );
    let t = json!([pay(
        json!([["set", "@entity.paid", "@config.cap"]]),
        json!([["=", "@entity.paid", null]]),
        None
    )]);
    assert_eq!(only(&configured(t, unset)), Verdict::Proved);
}

#[test]
fn a_built_batch_that_never_writes_the_entity_owes_its_invariant_nothing() {
    let inv = row_invariant(json!([">=", "@entity.total", 0]));
    let elsewhere = json!([{ "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "batch",
        ["array/concat", [["create", "Ledger", { "n": 1 }]], ["array/map", "@entity.items", ["fn", "i", ["update", "Ledger", "@i.id", { "n": 2 }]]]]]] }]);
    assert!(
        prove(&program(elsewhere, J::Null, inv.clone())).is_empty(),
        "the batch writes only Ledger"
    );
    let here = json!([{ "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "batch",
        ["array/map", "@entity.items", ["fn", "i", ["update", "Order", "@i.id", { "total": "@i.total" }]]]]] }]);
    assert!(
        prove(&program(here, J::Null, inv.clone()))
            .iter()
            .all(|f| matches!(f.verdict, Verdict::Unproved { .. })),
        "it writes Order with data the prover cannot follow"
    );
    let opaque = json!([{ "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "batch", "@payload.ops"]] }]);
    assert!(
        matches!(
            only(&program(opaque, J::Null, inv)),
            Verdict::Unproved { .. }
        ),
        "control: a batch it cannot follow may write anything"
    );
}

#[test]
fn another_traits_write_is_unproved_not_violated() {
    let mut schema = program(
        json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.status", "paid"]] }]),
        J::Null,
        state_holds("paid", json!([">=", "@entity.total", 0])),
    );
    let refund: orbital_core_base::schema::types::TraitDefinition = serde_json::from_value(json!({
        "name": "Refund", "linkedEntity": "Order",
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
            "events": [{ "key": "REFUND", "name": "Refund" }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "REFUND",
                "effects": [["persist", "update", "Order", "@entity.id", { "total": -1 }]] }] }
    }))
    .unwrap();
    schema.orbitals[0]
        .traits
        .push(orbital_core_base::schema::types::TraitReference::Inline(
            refund,
        ));
    let v = verdicts(&schema);
    let foreign = v
        .iter()
        .find(|(s, _)| s.contains("Refund"))
        .expect("foreign obligation");
    let Verdict::Unproved {
        counterexample: Some(_),
        ..
    } = &foreign.1
    else {
        panic!("{v:#?}")
    };
}

#[test]
fn a_config_bound_target_is_resolved_and_an_unknown_one_is_unproved() {
    use orbital_core_base::schema::types::{ConfigField, TraitReference};
    let inv = row_invariant(json!([">=", "@entity.total", 0]));
    let with_target = |default: J, effects: J| {
        let mut schema = program(
            json!([{ "from": "open", "to": "open", "event": "ADD", "effects": effects }]),
            J::Null,
            inv.clone(),
        );
        if let TraitReference::Inline(td) = &mut schema.orbitals[0].traits[0] {
            let field: ConfigField =
                serde_json::from_value(json!({ "type": "entity", "default": default }))
                    .unwrap();
            td.config = Some(orbital_core_base::collections::IndexMap::from([(
                "target".to_string(),
                field,
            )]));
        }
        schema
    };
    let create = json!([["persist", "create", "@config.target", { "total": -1 }]]);
    assert!(matches!(
        only(&with_target(json!("Order"), create.clone())),
        Verdict::Violated { .. }
    ));
    let Verdict::Unproved { reason, .. } =
        only(&with_target(json!("@config.elsewhere"), create))
    else {
        panic!()
    };
    assert!(reason.contains("not statically known"), "{reason}");
    let computed = json!([["persist", "batch", ["array/concat", "@payload.ops", []]]]);
    assert!(matches!(
        only(&with_target(json!("Order"), computed)),
        Verdict::Unproved { .. }
    ));
    let other = json!([["persist", "create", "@config.target", { "total": -1 }]]);
    assert!(
        prove(&with_target(json!("Invoice"), other)).is_empty(),
        "control: a resolved target that is another entity owes nothing"
    );
}

/// Soundness against the evaluator: every implication the solver proves holds on seeded random ground values
/// (nulls included), evaluated by the real evaluator.
#[test]
fn every_proved_implication_holds_in_the_evaluator() {
    use super::decide::{decide, Model, ModelValue, Outcome};
    use super::formula::{Formula, Sort, Translator, VarInfo, Vars};
    use super::rat::Rat;
    use orbital_core_base::schema::sexpr::SExpression;
    let var = |sort: Sort, nullable: bool| VarInfo { sort, nullable };
    let status = Sort::Str(Some(vec!["open".into(), "paid".into()]));
    let vars: Vars = [
        ("@entity.a".to_string(), var(Sort::Num, true)),
        ("@entity.b".to_string(), var(Sort::Num, false)),
        ("@payload.c".to_string(), var(Sort::Num, true)),
        ("@entity.s".to_string(), var(status, true)),
        ("@entity.t".to_string(), var(Sort::Str(None), true)),
        ("@entity.f".to_string(), var(Sort::Bool, true)),
    ]
    .into_iter()
    .collect();
    let cases: Vec<(J, J)> = vec![
        (
            json!(["<=", "@entity.a", "@entity.b"]),
            json!(["<=", ["-", "@entity.a", 1], "@entity.b"]),
        ),
        (
            json!([
                "and",
                ["<=", ["+", "@entity.a", "@payload.c"], "@entity.b"],
                [">=", "@payload.c", 0]
            ]),
            json!(["<=", "@entity.a", "@entity.b"]),
        ),
        (
            json!(["=", "@entity.a", null]),
            json!([">=", "@entity.a", 0]),
        ),
        (
            json!(["=", "@entity.a", null]),
            json!(["!=", "@entity.a", 0]),
        ),
        (
            json!(["!=", "@entity.s", "open"]),
            json!(["or", ["=", "@entity.s", "paid"], ["=", "@entity.s", null]]),
        ),
        (
            json!(["=", "@entity.t", "@entity.s"]),
            json!(["or", ["=", "@entity.t", null], ["!=", "@entity.t", "x"]]),
        ),
        (json!("@entity.f"), json!(["=", "@entity.f", true])),
        (
            json!(["not", "@entity.f"]),
            json!(["or", ["=", "@entity.f", false], ["=", "@entity.f", null]]),
        ),
        (
            json!(["and", [">", "@entity.a", 1], ["<", "@entity.a", 3]]),
            json!(["!=", ["*", 2, "@entity.a"], 7]),
        ),
        (
            json!([
                "if",
                [">", "@payload.c", 0],
                ["=", "@entity.b", 1],
                ["=", "@entity.b", 2]
            ]),
            json!(["and", [">=", "@entity.b", 1], ["<=", "@entity.b", 2]]),
        ),
        (
            json!(["let", [["d", ["+", "@entity.b", 2]]], [">", "@d", 4]]),
            json!([">", "@entity.b", 2]),
        ),
        (
            json!(true),
            json!([">=", ["if", [">", "@entity.a", 0], "@entity.a", 0], 0]),
        ),
        (
            json!([">", "@entity.b", 0]),
            json!(["=", ["if", [">", "@entity.b", 0], "x", "y"], "x"]),
        ),
        (
            json!([">=", "@entity.a", 0]),
            json!([">=", ["default", "@entity.a", 7], 0]),
        ),
        (
            json!(true),
            json!([">=", ["math/max", 0, ["-", "@entity.b", 100]], 0]),
        ),
        (json!(true), json!(["<=", ["math/min", 5, "@entity.b"], 5])),
        (
            json!(true),
            json!([">=", ["math/abs", ["-", "@entity.b", 3]], 0]),
        ),
        (
            json!([">", "@entity.b", 3]),
            json!([
                "=",
                ["math/abs", ["-", "@entity.b", 3]],
                ["-", "@entity.b", 3]
            ]),
        ),
        (
            json!([">", "@entity.b", 2]),
            json!([">=", ["math/max", "@entity.b", 3], 3]),
        ),
        (
            json!(true),
            json!([
                "<=",
                [
                    "+",
                    ["if", [">", "@entity.b", 0], 1, 0],
                    ["if", ["<", "@entity.b", 5], 1, 0]
                ],
                2
            ]),
        ),
        (
            json!(["and", [">", "@entity.b", 0], ["<", "@entity.b", 5]]),
            json!([
                "=",
                [
                    "+",
                    ["if", [">", "@entity.b", 0], 1, 0],
                    ["if", ["<", "@entity.b", 5], 1, 0]
                ],
                2
            ]),
        ),
    ];
    let mut seed: u64 = 0x5eed;
    let mut next = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    let nums = [
        None,
        Some(-2.0),
        Some(-1.0),
        Some(0.0),
        Some(0.5),
        Some(1.0),
        Some(2.0),
        Some(3.0),
        Some(3.5),
        Some(4.0),
        Some(5.0),
    ];
    let strs = [None, Some("open"), Some("paid"), Some("x"), Some("")];
    let bools = [None, Some(true), Some(false)];
    for (assume, goal) in cases {
        let (assume, goal): (SExpression, SExpression) = (
            serde_json::from_value(assume).unwrap(),
            serde_json::from_value(goal).unwrap(),
        );
        let mut t = Translator::new(&vars);
        let f = Formula::and(vec![
            t.truthy(&assume).unwrap(),
            t.falsy(&goal).unwrap(),
        ]);
        assert_eq!(
            decide(&f, &vars),
            Outcome::Unsat,
            "{assume:?} => {goal:?} should be proved"
        );
        for _ in 0..2000 {
            let mut model = Model::default();
            for (name, info) in &vars {
                let value = match &info.sort {
                    Sort::Num => match nums[next(nums.len())] {
                        Some(n) => ModelValue::Num(Rat::from_f64(n).unwrap()),
                        None if info.nullable => ModelValue::Null,
                        None => ModelValue::Num(Rat::ZERO),
                    },
                    Sort::Str(domain) => match strs[next(strs.len())] {
                        Some(v) if domain.as_ref().is_none_or(|d| d.iter().any(|x| x == v)) => {
                            ModelValue::Str(v.to_string())
                        }
                        _ if info.nullable => ModelValue::Null,
                        _ => ModelValue::Str("open".to_string()),
                    },
                    Sort::Bool => match bools[next(bools.len())] {
                        Some(b) => ModelValue::Bool(b),
                        None => ModelValue::Null,
                    },
                };
                model.values.insert(name.clone(), value);
            }
            assert!(
                !confirm::confirms(&model, std::slice::from_ref(&assume), &goal),
                "unsound: {assume:?} => {goal:?} fails on {model:?}"
            );
        }
    }
}

/// The findings of one proposition kind only.
fn of_kind(schema: &OrbitalSchema, transition: bool) -> Vec<(String, Verdict)> {
    prove(schema)
        .into_iter()
        .filter(|f| matches!(f.proposition, Proposition::Transition { .. }) == transition)
        .map(|f| (f.site, f.verdict))
        .collect()
}

fn pay(effects: J, ensures: J, guard: Option<J>) -> J {
    let mut t = json!({ "from": "open", "to": "paid", "event": "PAY", "effects": effects, "ensures": ensures });
    if let Some(g) = guard {
        t["guard"] = g;
    }
    t
}

#[test]
fn an_ensures_relating_the_new_row_to_the_old_one_is_proved() {
    let ensures = json!([[
        "=",
        "@entity.paid",
        ["+", "@fromEntity.paid", "@payload.amount"]
    ]]);
    let t = json!([pay(
        json!([[
            "set",
            "@entity.paid",
            ["+", "@entity.paid", "@payload.amount"]
        ]]),
        ensures,
        None
    )]);
    assert_eq!(only(&program(t, J::Null, J::Null)), Verdict::Proved);
}

#[test]
fn an_off_by_one_write_breaks_the_ensures_with_a_confirmed_counterexample() {
    let ensures = json!([[
        "=",
        "@entity.paid",
        ["+", "@fromEntity.paid", "@payload.amount"]
    ]]);
    let t = json!([pay(
        json!([[
            "set",
            "@entity.paid",
            ["+", ["+", "@entity.paid", "@payload.amount"], 1]
        ]]),
        ensures,
        None
    )]);
    // `x + 1 = x` fails for every input, so any row is a counterexample.
    assert!(matches!(
        only(&program(t, J::Null, J::Null)),
        Verdict::Violated { .. }
    ));
}

#[test]
fn a_nullable_ensures_is_proved_by_a_presence_guard_and_refuted_only_by_present_values() {
    let ensures = json!([[">=", "@entity.paid", 0]]);
    let keep = json!([["set", "@entity.status", "paid"]]);
    let guarded = pay(
        keep.clone(),
        ensures.clone(),
        Some(json!([">=", "@entity.paid", 0])),
    );
    assert_eq!(
        only(&program(json!([guarded]), J::Null, J::Null)),
        Verdict::Proved
    );
    let bare = json!([pay(keep.clone(), ensures.clone(), None)]);
    let Verdict::Violated { counterexample } = only(&program(bare, J::Null, J::Null)) else {
        panic!("without the guard a negative paid breaks the ensures")
    };
    let paids: Vec<&J> = counterexample
        .as_object()
        .unwrap()
        .values()
        .filter_map(|root| root.get("paid"))
        .collect();
    assert!(
        !paids.is_empty() && paids.iter().all(|p| p.as_f64().is_some_and(|n| n < 0.0)),
        "a null paid raises instead of violating: {counterexample}"
    );
    let null_only = pay(keep, ensures, Some(json!(["=", "@entity.paid", null])));
    assert_eq!(
        only(&program(json!([null_only]), J::Null, J::Null)),
        Verdict::Proved,
        "an ordering over a null raises: the ensures never evaluates to false"
    );
}

#[test]
fn an_opaque_write_leaves_the_ensures_unproved_never_passed() {
    let ensures = json!([[">=", "@entity.paid", 0]]);
    let t = json!([pay(
        json!([["set", "@entity.paid", ["str/len", "@payload.status"]]]),
        ensures,
        None
    )]);
    let Verdict::Unproved { reason, .. } = only(&program(t, J::Null, J::Null)) else {
        panic!()
    };
    assert!(reason.contains("str/len"), "{reason}");
}

#[test]
fn an_own_row_update_is_what_the_ensures_reads_and_its_control_fails() {
    let write = json!([["persist", "update", "Order", "@entity.id", { "paid": ["+", "@entity.paid", "@payload.amount"] }]]);
    let grows = json!([[
        "=",
        "@entity.paid",
        ["+", "@fromEntity.paid", "@payload.amount"]
    ]]);
    assert_eq!(
        only(&program(
            json!([pay(write.clone(), grows, None)]),
            J::Null,
            J::Null
        )),
        Verdict::Proved
    );
    let unchanged = json!([["=", "@entity.paid", "@fromEntity.paid"]]);
    assert!(matches!(
        only(&program(
            json!([pay(write, unchanged, None)]),
            J::Null,
            J::Null
        )),
        Verdict::Violated { .. }
    ));
}

#[test]
fn the_ensures_assumes_the_guard_and_the_source_states_holds() {
    let ensures = json!([[">=", "@entity.paid", 0]]);
    let spend = json!([[
        "set",
        "@entity.paid",
        ["+", "@entity.paid", "@payload.amount"]
    ]]);
    let guard = json!([">=", "@payload.amount", 0]);
    let mut schema = program(
        json!([pay(spend.clone(), ensures.clone(), Some(guard.clone()))]),
        J::Null,
        J::Null,
    );
    assert!(
        matches!(of_kind(&schema, true)[..], [(_, Verdict::Violated { .. })]),
        "without the source state's @holds the old row may be negative"
    );
    let TraitReference::Inline(td) = &mut schema.orbitals[0].traits[0] else {
        panic!()
    };
    td.state_machine.states[0].holds =
        Some(serde_json::from_value(json!([">=", "@entity.paid", 0])).unwrap());
    assert_eq!(
        of_kind(&schema, true),
        vec![(of_kind(&schema, true)[0].0.clone(), Verdict::Proved)]
    );
    let unguarded = program(json!([pay(spend, ensures, None)]), J::Null, J::Null);
    assert!(
        matches!(
            of_kind(&unguarded, true)[..],
            [(_, Verdict::Violated { .. })]
        ),
        "control: the guard is what keeps it"
    );
}

#[test]
fn a_spec_ensures_and_the_same_body_ensures_owe_one_obligation() {
    let ensures = json!([[
        "=",
        "@entity.paid",
        ["+", "@fromEntity.paid", "@payload.amount"]
    ]]);
    let spec = json!([{ "kind": "transition", "traitName": "Checkout", "from": "open", "event": "PAY", "to": "paid", "ensures": ensures, "strict": true }]);
    let t = json!([pay(
        json!([[
            "set",
            "@entity.paid",
            ["+", "@entity.paid", "@payload.amount"]
        ]]),
        ensures,
        None
    )]);
    assert_eq!(only(&program(t, J::Null, spec.clone())), Verdict::Proved);
    let bare = json!([{ "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.paid", 0]] }]);
    assert!(
        matches!(
            only(&program(bare, J::Null, spec)),
            Verdict::Violated { .. }
        ),
        "a spec ensures binds a body without one"
    );
}

#[test]
fn a_body_invariant_and_a_body_holds_are_owed_like_spec_ones() {
    let t = json!([{ "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "create", "Order", { "total": "@payload.total" }]] }]);
    let mut schema = program(t, J::Null, J::Null);
    let EntityRef::Inline(def) = &mut schema.orbitals[0].entity else {
        panic!()
    };
    def.invariants = Some(vec![serde_json::from_value(json!([
        ">=",
        "@entity.total",
        0
    ]))
    .unwrap()]);
    assert!(matches!(only(&schema), Verdict::Violated { .. }));
    let control = program(
        json!([{ "from": "open", "to": "open", "event": "ADD", "effects": [["persist", "create", "Order", { "total": "@payload.total" }]] }]),
        J::Null,
        J::Null,
    );
    assert!(
        prove(&control).is_empty(),
        "control: no proposition, no obligation"
    );
}

#[test]
fn a_transition_that_does_not_carry_the_ensures_owes_nothing() {
    let ensures = json!([["=", "@entity.paid", 0]]);
    let t = json!([
        pay(json!([["set", "@entity.paid", 0]]), ensures, None),
        { "from": "open", "to": "open", "event": "ADD", "effects": [["set", "@entity.paid", 7]] }
    ]);
    let v = verdicts(&program(t, J::Null, J::Null));
    assert_eq!(v.len(), 1, "{v:#?}");
    assert!(v[0].0.contains("PAY"), "{v:#?}");
}

// G-ORB-535: std-lifecycle's two `ScanRecordsLoaded` arms share from/event/to; under `action: delete` the status arm's
// `@ensures` was checked against the live delete arm.
#[test]
fn an_arms_ensures_binds_that_arm_not_a_sibling_sharing_its_event() {
    let action = json!({ "action": { "type": "string", "default": "delete" } });
    let status_arm = pay(
        json!([["set", "@entity.paid", 3]]),
        json!([["=", "@entity.paid", 3]]),
        Some(json!(["=", "@config.action", "status"])),
    );
    let mut delete_arm = pay(json!([["set", "@entity.paid", 0]]), json!(null), Some(json!(["=", "@config.action", "delete"])));
    delete_arm.as_object_mut().unwrap().remove("ensures");
    assert_eq!(only(&configured(json!([status_arm, delete_arm]), action)), Verdict::Proved);
}

#[test]
fn control_an_expected_transition_still_binds_every_arm_with_its_event() {
    let ensures = json!([["=", "@entity.paid", 3]]);
    let arms = json!([
        { "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.paid", 3]] },
        { "from": "open", "to": "paid", "event": "PAY", "effects": [["set", "@entity.paid", 0]] }
    ]);
    let spec = json!([{ "kind": "transition", "traitName": "Checkout", "from": "open", "event": "PAY", "to": "paid", "ensures": ensures, "strict": true }]);
    let v = verdicts(&program(arms, J::Null, spec));
    assert!(v.iter().any(|(_, x)| matches!(x, Verdict::Violated { .. })), "{v:#?}");
}

// G-ORB-535: a call site's `enabled: (> @config.keepDays 0)` reaches the trait folded to `(> 30 0)`.
#[test]
fn a_closed_call_site_default_is_folded_not_unresolved() {
    let enabled = json!({ "enabled": { "type": "boolean", "default": [">", 30, 0] } });
    let t = json!([pay(
        json!([["set", "@entity.paid", 3]]),
        json!([["and", "@config.enabled", ["=", "@entity.paid", 3]]]),
        None
    )]);
    assert_eq!(only(&configured(t, enabled)), Verdict::Proved);
}

#[test]
fn control_a_default_bound_at_run_time_stays_unresolved() {
    let enabled = json!({ "enabled": { "type": "boolean", "default": [">", "@user.level", 0] } });
    let t = json!([pay(
        json!([["set", "@entity.paid", 3]]),
        json!([["and", "@config.enabled", ["=", "@entity.paid", 3]]]),
        None
    )]);
    assert!(matches!(only(&configured(t, enabled)), Verdict::Unproved { .. }));
}
