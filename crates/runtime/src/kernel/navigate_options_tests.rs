use super::*;

/// §157 `navigate-route-params`: the third `navigate` argument's `crumb` must
/// resolve through the same `BindingContext` as `params`, not stay the
/// unresolved `@payload.title` literal.
#[test]
fn navigate_carries_crumb_options() {
    let mut payload = ValueMap::default();
    payload.insert("title".into(), Value::string("My Note"));
    let ctx = BindingContextBuilder::new()
        .payload(Value::object(payload))
        .build();

    let effect = Effect::Navigate {
        path: "/notes/:id".to_string(),
        params: None,
        options: Some(orbital_core_base::client_effect::NavigateOptions {
            crumb: Some("@payload.title".to_string()),
        }),
    };

    let resolved = resolve_effect_bindings(effect, &ctx).expect("resolves");
    let Effect::Navigate { options, .. } = resolved else {
        panic!("expected Navigate effect");
    };
    assert_eq!(
        options.and_then(|o| o.crumb),
        Some("My Note".to_string()),
        "the crumb must resolve from @payload.title, not stay the literal token"
    );
}

/// EXEC-EVAL-4: a mistyped operator call in navigate params is the effect's failure, as in render props.
#[test]
fn a_mistyped_call_in_navigate_params_fails_resolution() {
    let expr: orbital_core_base::schema::sexpr::SExpression = serde_json::from_value(serde_json::json!(
        ["navigate", "/items", { "tags": ["str/join", ["array/filter", "not-a-list", ["fn", "x", true]], ", "] }]
    ))
    .unwrap();
    let effect = orbital_core_base::effect_types::parse_effect(&expr).expect("a navigate effect");
    let resolved = resolve_effect_bindings(effect, &BindingContextBuilder::new().build());
    assert!(resolved.is_err(), "{resolved:?}");
}

#[test]
fn control_a_well_typed_call_in_navigate_params_resolves_to_its_value() {
    let expr: orbital_core_base::schema::sexpr::SExpression = serde_json::from_value(serde_json::json!(
        ["navigate", "/items", { "tags": ["str/join", ["array/filter", ["a", "b"], ["fn", "x", true]], ", "] }]
    ))
    .unwrap();
    let effect = orbital_core_base::effect_types::parse_effect(&expr).expect("a navigate effect");
    let Ok(Effect::Navigate { params: Some(params), .. }) = resolve_effect_bindings(effect, &BindingContextBuilder::new().build()) else {
        panic!("expected resolved navigate params");
    };
    assert_eq!(params.get("tags"), Some(&Value::string("a, b")));
}
