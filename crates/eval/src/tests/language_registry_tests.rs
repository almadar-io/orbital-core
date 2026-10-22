#[allow(unused_imports)]
use orbital_core_base::language::registry::*;

// Operator-parity gate (zero exemptions): the runtime evaluator
// (`crate::evaluator::get_operator`) must resolve EVERY operator whose
// canonical category targets `rust`. Agent ops are stubbed, so they count.
// Fails when a canonical rust-target op lands without a Rust impl/registration.
#[test]
fn rust_evaluator_covers_every_canonical_rust_operator() {
    let doc: serde_json::Value = serde_json::from_str(operators_json()).unwrap();
    let rust_cats: std::collections::HashSet<String> = doc["categories"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, m)| {
            m["target"]
                .as_array()
                .map(|t| t.iter().any(|x| x.as_str() == Some("rust")))
                .unwrap_or(false)
        })
        .map(|(c, _)| c.clone())
        .collect();

    let mut missing = Vec::new();
    for (name, meta) in doc["operators"].as_object().unwrap() {
        let cat = meta["category"].as_str().unwrap_or("");
        if !rust_cats.contains(cat) {
            continue;
        }
        if crate::evaluator::operators::get_operator(name).is_none() {
            missing.push(format!("{name} ({cat})"));
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "Rust evaluator missing {} canonical rust-target operators:\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
}

#[test]
fn operator_metadata_is_populated() {
    let plus = get_operator("+").expect("'+' must exist");
    assert_eq!(plus.category, "arithmetic");
    assert_eq!(plus.min_arity, 2);
    assert!(!plus.has_side_effects);

    let set = get_operator("set").expect("'set' must exist");
    assert!(set.has_side_effects);
}

#[test]
fn fetch_operator_produces_the_data_wrapper_it_actually_emits() {
    // `effect.produces` is defined as "the payload the runtime puts on the
    // bus when emit.success fires". For fetch that payload is the WRAPPER
    // `{ data, totalCount }` — verified in both executors that emit onto
    // the bus: EffectExecutor.ts:851-855 and
    // orbital-shell-typescript codegen/effect/server.rs:177-185.
    //
    // It previously declared a bare `entity | [entity]`, i.e. the wrapper
    // was real but undeclared — the mirror image of the persist bug where a
    // wrapper was declared but unreal. `data` keeps the entity/array union
    // inside the wrapper so a by-id vs collection call site still narrows.
    let fetch = get_operator("fetch").expect("fetch must exist (V2 Phase 0)");
    let effect = fetch
        .effect
        .as_ref()
        .expect("fetch must carry effect metadata");
    assert_eq!(effect.kind, "fetch");
    let produces = effect.produces.as_ref().expect("fetch produces present");
    let OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, .. }) = produces else {
        panic!("fetch produces should be the {{data,totalCount}} Object, got {produces:?}");
    };
    assert!(fields.contains_key("data"), "fetch emits a `data` key");
    assert!(
        fields.contains_key("totalCount"),
        "fetch emits `totalCount` alongside `data`"
    );
    match fields.get("data").expect("data field") {
        OperatorTypeRef::Tagged(OperatorTypeRefKind::Union { of }) => {
            assert_eq!(of.len(), 2, "data is entity | Array<entity>");
        }
        other => panic!("fetch produces.data should stay a Union, got {other:?}"),
    }
}

#[test]
fn ref_operator_produces_the_same_wrapper_as_fetch() {
    // `ref` emits the identical wrapper — and fires it on `on_change`, not
    // `success` (EffectExecutor.ts:919-923, codegen server.rs:248-253).
    let r = get_operator("ref").expect("ref must exist");
    let produces = r
        .effect
        .as_ref()
        .expect("ref effect")
        .produces
        .as_ref()
        .expect("ref produces");
    let OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, .. }) = produces else {
        panic!("ref produces should be an Object wrapper, got {produces:?}");
    };
    assert!(fields.contains_key("data"));
    assert!(fields.contains_key("totalCount"));
}

#[test]
fn math_operators_have_no_effect_block() {
    // Operators that aren't in the V2 Phase 0 scope keep effect = None.
    // Confirms #[serde(default)] falls through cleanly for pre-v2 entries.
    let plus = get_operator("+").expect("'+' must exist");
    assert!(plus.effect.is_none());
    let abs = get_operator("math/abs").expect("math/abs must exist");
    assert!(abs.effect.is_none());
}
