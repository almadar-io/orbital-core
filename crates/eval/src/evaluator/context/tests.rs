use super::*;

fn sample_payload() -> Value {
    let mut data = ValueMap::default();
    data.insert(
        "email".into(),
        Value::string("test@example.com"),
    );
    data.insert("name".into(), Value::string("Test User"));
    data.insert("connectionSlots".into(), Value::Number(10.0));

    let mut payload = ValueMap::default();
    payload.insert("data".into(), Value::object(data));
    payload.insert("id".into(), Value::string("user-123"));

    Value::object(payload)
}

#[test]
fn a_render_binding_to_a_missing_field_of_a_held_root_resolves_to_null() {
    let mut ctx = BindingContext::new();
    let mut user = ValueMap::default();
    user.insert("email".into(), Value::string("learner@kflow.test"));
    ctx.set_user(Value::object(user));
    let mut card = ValueMap::default();
    card.insert("name".into(), Value::string("@user.name"));
    card.insert("email".into(), Value::string("@user.email"));
    let resolved = ctx.resolve_value(&Value::object(card)).into_owned();
    let Value::Object(out) = resolved else { panic!("an object resolves to an object") };
    assert_eq!(out.get("name"), Some(&Value::Null));
    assert_eq!(out.get("email"), Some(&Value::string("learner@kflow.test")));
}

#[test]
fn control_a_binding_to_a_root_the_kernel_has_not_populated_stays_its_token() {
    let ctx = BindingContext::new();
    let token = Value::string("@pages");
    assert_eq!(ctx.resolve_value(&token).into_owned(), token);
    let theme = Value::string("@currentTheme.name");
    assert_eq!(ctx.resolve_value(&theme).into_owned(), theme);
}

fn sample_user() -> Value {
    let mut user = ValueMap::default();
    user.insert(
        "id".into(),
        Value::string("current-user-id"),
    );
    user.insert(
        "name".into(),
        Value::string("Current User"),
    );
    Value::object(user)
}

// G-ORBITAL-CORE-033 (ruled 2026-10-18): an operator call in an effect argument that fails is a failure, never data.
fn untranslatable() -> Value {
    Value::array(vec![Value::String(("math/abs".to_string()).into())])
}

#[test]
fn a_failing_operator_call_faults_the_checked_resolution() {
    let ctx = BindingContextBuilder::new().build();
    assert!(ctx.resolve_value_checked(&untranslatable()).is_err());
}

#[test]
fn control_a_working_operator_call_resolves_under_the_checked_resolution() {
    let ctx = BindingContextBuilder::new().build();
    let sum = Value::array(vec![Value::String(("+".to_string()).into()), Value::Number(1.0), Value::Number(2.0)]);
    assert_eq!(ctx.resolve_value_checked(&sum).unwrap().into_owned(), Value::Number(3.0));
}

#[test]
fn edge_the_unchecked_resolution_still_keeps_a_failing_call() {
    let ctx = BindingContextBuilder::new().build();
    assert_eq!(ctx.resolve_value(&untranslatable()).into_owned(), untranslatable());
}

#[test]
fn test_resolve_simple_binding() {
    let ctx = BindingContextBuilder::new()
        .payload(sample_payload())
        .build();

    let result = ctx.resolve("payload.data.email");
    assert_eq!(result, Some(Value::string("test@example.com")));
}

#[test]
fn test_resolve_nested_path() {
    let ctx = BindingContextBuilder::new()
        .payload(sample_payload())
        .build();

    let result = ctx.resolve("payload.data.connectionSlots");
    assert_eq!(result, Some(Value::Number(10.0)));
}

#[test]
fn test_resolve_value_every_root() {
    // THE canonical value-walker resolves any `@payload.X` / `@entity.X` / `@config.X`
    // inside a tree — no per-binding special-casing. (Effects run through this.)
    let mut entity = ValueMap::default();
    entity.insert("title".into(), Value::string("Ticket #1"));
    let mut config = ValueMap::default();
    config.insert("look".into(), Value::string("striped"));
    let ctx = BindingContextBuilder::new()
        .payload(sample_payload())
        .entity(Value::object(entity))
        .config(Value::object(config))
        .build();

    let content = Value::object(ValueMap::from_iter([
        // a data table bound to the loaded collection — resolves to the payload value
        (
            "rows".into(),
            Value::String(("@payload.data".to_string()).into()),
        ),
        (
            "heading".into(),
            Value::String(("@entity.title".to_string()).into()),
        ),
        (
            "look".into(),
            Value::String(("@config.look".to_string()).into()),
        ),
        (
            "missing".into(),
            Value::String(("@payload.nope".to_string()).into()),
        ),
        ("literal".into(), Value::String(("plain".to_string()).into())),
    ]));
    let resolved = ctx.resolve_value(&content).into_owned();
    let obj = resolved.as_object().unwrap();
    assert!(
        obj["rows"].as_object().is_some(),
        "@payload.data → the data object"
    );
    assert_eq!(obj["heading"], Value::string("Ticket #1"));
    assert_eq!(obj["look"], Value::string("striped"));
    // A missing field of a held root reads null, never its own source text.
    assert_eq!(obj["missing"], Value::Null);
    assert_eq!(obj["literal"], Value::string("plain"));
}

/// A render tree resolves `i18n/t` against the context's locale and catalog
/// (the dispatch viewer's), params included.
#[test]
fn resolve_value_evaluates_a_translate_call_with_the_contexts_catalog() {
    let mut entity = ValueMap::default();
    entity.insert("count".into(), Value::Number(3.0));
    let ctx = BindingContextBuilder::new()
        .entity(Value::object(entity))
        .locale(Value::string("ar"))
        .messages(HashMap::from([(
            "home:posts.count".to_string(),
            "{{n}} منشورات".to_string(),
        )]))
        .build();
    let call = Value::array(vec![
        Value::String(("i18n/t".to_string()).into()),
        Value::String(("home:posts.count".to_string()).into()),
        Value::Object(ValueMap::from_iter([(
            "n".into(),
            Value::String(("@entity.count".to_string()).into()),
        )]).into()),
    ]);
    let resolved = ctx
        .resolve_value(&Value::object(ValueMap::from_iter([(
            "label".into(),
            call,
        )])))
        .into_owned();
    assert_eq!(
        resolved.as_object().unwrap()["label"],
        Value::string("3 منشورات")
    );
}

#[test]
fn control_resolve_value_still_evaluates_other_operators() {
    let ctx = BindingContextBuilder::new().build();
    let call = Value::array(vec![
        Value::String(("str/concat".to_string()).into()),
        Value::String(("/ar".to_string()).into()),
        Value::String(("/blog".to_string()).into()),
    ]);
    assert_eq!(
        ctx.resolve_value(&call).into_owned(),
        Value::string("/ar/blog")
    );
}

#[test]
fn test_resolve_value_config_forward_to_another_binding() {
    // A declared config default can itself be a bare binding token forwarding
    // to a render-resolved sigil (`theme : string = @currentTheme` on
    // std-app-layout) or another root (`viewerName = @user.name`). The first
    // hop resolves `@config.theme` to the STRING `"@currentTheme"`; without a
    // second hop that scalar round-trips literal on the wire (the exact §155.1
    // gap: `box.data-theme` arriving as `"@currentTheme"`).
    let mut config = ValueMap::default();
    config.insert(
        "theme".into(),
        Value::string("@currentTheme"),
    );
    let mut user = ValueMap::default();
    user.insert("name".into(), Value::string("Ada"));
    let ctx = BindingContextBuilder::new()
        .config(Value::object(config))
        .current_theme(Value::string("clay-light"))
        .user(Value::object(user))
        .build();

    let forward = Value::string("@config.theme");
    let resolved = ctx.resolve_value(&forward);
    assert_eq!(
        resolved.into_owned(),
        Value::string("clay-light"),
        "a config default forwarding to @currentTheme must resolve through BOTH hops"
    );
}

#[test]
fn test_resolve_value_runtime_data_is_never_evaluated_as_code() {
    let mut payload = ValueMap::default();
    payload.insert(
        "argv".into(),
        Value::array(vec![Value::String("list".into()), Value::String("-c".into()), Value::String("x".into())]),
    );
    payload.insert("note".into(), Value::string("@user.email"));
    let mut entity = ValueMap::default();
    entity.insert(
        "form".into(),
        Value::array(vec![Value::String("if".into()), Value::Bool(true), Value::String("a".into()), Value::String("b".into())]),
    );
    let mut user = ValueMap::default();
    user.insert("email".into(), Value::string("ada@example.com"));
    let ctx = BindingContextBuilder::new()
        .payload(Value::object(payload))
        .entity(Value::object(entity))
        .user(Value::object(user))
        .build();
    assert_eq!(
        ctx.resolve_value(&Value::string("@payload.argv")).into_owned(),
        Value::array(vec![Value::String("list".into()), Value::String("-c".into()), Value::String("x".into())]),
        "a payload array whose head names an operator is data, not a `list` call"
    );
    assert_eq!(
        ctx.resolve_value(&Value::string("@entity.form")).into_owned(),
        Value::array(vec![Value::String("if".into()), Value::Bool(true), Value::String("a".into()), Value::String("b".into())]),
        "an entity value shaped like an `if` form is data"
    );
    assert_eq!(
        ctx.resolve_value(&Value::string("@payload.note")).into_owned(),
        Value::string("@user.email"),
        "a payload string that looks like a binding stays the caller's text"
    );
}

#[test]
fn test_resolve_value_config_trees_and_authored_forms_still_evaluate() {
    let mut config = ValueMap::default();
    config.insert(
        "label".into(),
        Value::array(vec![Value::String("if".into()), Value::Bool(true), Value::String("A".into()), Value::String("B".into())]),
    );
    let ctx = BindingContextBuilder::new().config(Value::object(config)).build();
    assert_eq!(
        ctx.resolve_value(&Value::string("@config.label")).into_owned(),
        Value::string("A"),
        "control: a config-held operator form is authored and evaluates"
    );
    let authored = Value::array(vec![Value::String("str/concat".into()), Value::String("a".into()), Value::String("b".into())]);
    assert_eq!(
        ctx.resolve_value(&authored).into_owned(),
        Value::string("ab"),
        "control: an operator form written in the program evaluates"
    );
}

#[test]
fn test_resolve_value_self_forward_stays_literal_not_infinite_loop() {
    // A config default that forwards to itself (`x = @config.x`, an authoring
    // bug) must not recurse forever — the same occurs-check that already
    // guards structural (Object/Array) re-expansion covers the scalar hop.
    let mut config = ValueMap::default();
    config.insert("x".into(), Value::string("@config.x"));
    let ctx = BindingContextBuilder::new()
        .config(Value::object(config))
        .build();
    let forward = Value::string("@config.x");
    let resolved = ctx.resolve_value(&forward);
    assert_eq!(
        resolved.into_owned(),
        Value::string("@config.x")
    );
}

#[test]
fn test_resolve_value_interpolates_embedded_bindings() {
    // Parity with the JS runtime's `interpolateString`: a binding embedded in a
    // longer string ("Count: @entity.count") interpolates, while a pure-binding
    // string ("@entity.count") resolves to its typed value. Locks the runtime/
    // codegen parity for prefixed bindings (the "Count: @entity.count" bug).
    let mut entity = ValueMap::default();
    entity.insert("count".into(), Value::Number(0.0));
    entity.insert("name".into(), Value::string("Counter"));
    let ctx = BindingContextBuilder::new()
        .entity(Value::object(entity))
        .build();

    // Embedded → interpolated string.
    let content = Value::string("Count: @entity.count");
    let resolved = ctx.resolve_value(&content).into_owned();
    assert_eq!(resolved, Value::string("Count: 0"));

    // Multiple tokens in one string.
    let content = Value::string("@entity.name = @entity.count");
    let resolved = ctx.resolve_value(&content).into_owned();
    assert_eq!(resolved, Value::string("Counter = 0"));

    // Pure binding → typed value (number stays a number).
    let content = Value::string("@entity.count");
    let resolved = ctx.resolve_value(&content).into_owned();
    assert_eq!(resolved, Value::Number(0.0));

    // Unresolved embedded token is left literal (not crashed, not blanked).
    let content = Value::string("x: @entity.missing");
    let resolved = ctx.resolve_value(&content).into_owned();
    assert_eq!(resolved, Value::string("x: @entity.missing"));

    // Client-only root (`@trait.*`) passes through verbatim.
    let content = Value::string("see @trait.Other");
    let resolved = ctx.resolve_value(&content).into_owned();
    assert_eq!(resolved, Value::string("see @trait.Other"));
}

#[test]
fn test_resolve_user() {
    let ctx = BindingContextBuilder::new().user(sample_user()).build();

    assert_eq!(
        ctx.resolve("user.id"),
        Some(Value::string("current-user-id"))
    );
}

/// `@currentUser` was retired in favour of the single `@user` sigil: it
/// resolved here and nowhere in the JS interpreter, so an author using it
/// got a working compiled app and a silently empty interpreted one.
#[test]
fn test_current_user_alias_is_retired() {
    let ctx = BindingContextBuilder::new().user(sample_user()).build();

    assert_eq!(ctx.resolve("currentUser.id"), None);
}

#[test]
fn test_resolve_binding_with_at_sign() {
    let ctx = BindingContextBuilder::new()
        .payload(sample_payload())
        .build();

    let result = ctx.resolve_binding("@payload.data.name");
    assert_eq!(result, Some(Value::string("Test User")));
}

#[test]
fn test_resolve_now() {
    let ctx = BindingContext::new();
    let result = ctx.resolve("now");

    match result {
        Some(Value::String(text)) => {
            assert!(text.ends_with('Z') && text.len() == 24, "{text}");
            assert!(text.as_str() > "2020-01-01" && text.as_str() < "2100-01-01", "{text}");
        }
        other => panic!("Expected an ISO-8601 instant, got {other:?}"),
    }
}

#[test]
fn test_local_bindings() {
    let mut ctx = BindingContext::new();
    ctx.set_local("myVar", Value::Number(42.0));

    let result = ctx.resolve("myVar");
    assert_eq!(result, Some(Value::Number(42.0)));
}

#[test]
fn test_child_context() {
    let ctx = BindingContextBuilder::new()
        .payload(sample_payload())
        .local("outer", Value::Number(1.0))
        .build();

    let mut child = ctx.child();
    child.set_local("inner", Value::Number(2.0));

    // Child should have both
    assert_eq!(child.resolve("outer"), Some(Value::Number(1.0)));
    assert_eq!(child.resolve("inner"), Some(Value::Number(2.0)));

    // Parent should only have outer
    assert_eq!(ctx.resolve("outer"), Some(Value::Number(1.0)));
    assert_eq!(ctx.resolve("inner"), None);
}

#[test]
fn test_nonexistent_path() {
    let ctx = BindingContextBuilder::new()
        .payload(sample_payload())
        .build();

    assert_eq!(ctx.resolve("payload.data.nonexistent"), None);
    assert_eq!(ctx.resolve("nonexistent"), None);
    assert_eq!(ctx.resolve("payload.nonexistent.deep"), None);
}

#[test]
fn test_entity_stores() {
    let connections = vec![
        {
            let mut c = ValueMap::default();
            c.insert(
                "requesterId".into(),
                Value::string("user-1"),
            );
            c.insert("status".into(), Value::string("accepted"));
            Value::object(c)
        },
        {
            let mut c = ValueMap::default();
            c.insert(
                "requesterId".into(),
                Value::string("user-1"),
            );
            c.insert("status".into(), Value::string("pending"));
            Value::object(c)
        },
    ];

    let ctx = BindingContextBuilder::new()
        .entity_store("Connection", connections.clone())
        .build();

    let stored = ctx.get_entities("Connection");
    assert!(stored.is_some());
    assert_eq!(stored.unwrap().len(), 2);
}

/// Regression pin (std-multi-party-transaction stack overflow): a runtime
/// value can contain its OWN binding token — `(set @entity.audit (concat
/// @entity.audit {…}))` on an entity whose `audit` was still the
/// unresolved literal stores `"@entity.audit"` inside the new array.
/// Structure re-resolution must occurs-check the expansion stack and
/// leave the self-token literal instead of recursing forever.
#[test]
fn resolve_value_self_referential_structure_terminates() {
    let mut entry = ValueMap::default();
    entry.insert("action".into(), Value::string("restarted"));
    let mut entity = ValueMap::default();
    entity.insert(
        "audit".into(),
        Value::array(vec![
            Value::String(("@entity.audit".to_string()).into()),
            Value::Object(entry.into()),
        ]),
    );
    let ctx = BindingContextBuilder::new()
        .entity(Value::object(entity))
        .build();

    let resolved = ctx
        .resolve_value(&Value::string("@entity.audit"))
        .into_owned();
    let Value::Array(items) = resolved else {
        panic!("expected the audit array");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        Value::string("@entity.audit"),
        "the self-token must stay literal, not re-expand"
    );
}

/// Mutual cycle (`@entity.a` ⇄ `@entity.b`) must terminate too, and the
/// same token in SIBLING positions (not its own ancestor) must still
/// resolve normally.
#[test]
fn resolve_value_mutual_cycle_terminates_and_siblings_still_resolve() {
    let mut entity = ValueMap::default();
    entity.insert(
        "a".into(),
        Value::array(vec![Value::String(("@entity.b".to_string()).into())]),
    );
    entity.insert(
        "b".into(),
        Value::array(vec![Value::String(("@entity.a".to_string()).into())]),
    );
    entity.insert("name".into(), Value::string("N"));
    let ctx = BindingContextBuilder::new()
        .entity(Value::object(entity))
        .build();

    // Terminates (the inner @entity.a stays literal at the cycle edge).
    let _ = ctx
        .resolve_value(&Value::string("@entity.a"))
        .into_owned();

    // Sibling repetition of one token is NOT a cycle: both resolve.
    let siblings = Value::array(vec![
        Value::String(("@entity.name".to_string()).into()),
        Value::String(("@entity.name".to_string()).into()),
    ]);
    let resolved = ctx.resolve_value(&siblings).into_owned();
    assert_eq!(
        resolved,
        Value::array(vec![
            Value::String(("N".to_string()).into()),
            Value::String(("N".to_string()).into()),
        ])
    );
}

/// §157 `expr-in-render-props`: an operator array embedded as a VALUE inside
/// render-ui content (`["if", ["==", "@entity.status", "draft"], "Draft",
/// "Published"]`) is a call, not a literal list — `resolve_value` must
/// evaluate it through the one evaluator against this same context, exactly
/// as `packages/almadar-runtime/src/BindingResolver.ts`'s `interpolateArray`
/// (`isSExpression` ~533-543) does on the JS side.
#[test]
fn render_ui_operator_forms_are_evaluated() {
    let mut entity = ValueMap::default();
    entity.insert("status".into(), Value::string("draft"));
    let ctx = BindingContextBuilder::new()
        .entity(Value::object(entity))
        .build();

    let content = Value::array(vec![
        Value::String(("if".to_string()).into()),
        Value::Array(vec![
            Value::String(("==".to_string()).into()),
            Value::String(("@entity.status".to_string()).into()),
            Value::String(("draft".to_string()).into()),
        ].into()),
        Value::String(("Draft".to_string()).into()),
        Value::String(("Published".to_string()).into()),
    ]);

    let resolved = ctx.resolve_value(&content).into_owned();
    assert_eq!(resolved, Value::string("Draft"));
}

/// §157 `expr-in-render-props`: a render-time lambda (`["fn", "item", body]`)
/// stays unevaluated — the JS runtime special-cases the exact same flat
/// three-arity form (`BindingResolver.ts` ~445-462) because `@almadar/ui`'s
/// renderer beta-reduces it per row at render time; evaluating it here would
/// collapse it into a non-serializable `Value::Lambda` before the renderer
/// ever sees each row.
#[test]
fn render_ui_lambdas_stay_unevaluated() {
    let ctx = BindingContext::new();
    let content = Value::array(vec![
        Value::String(("fn".to_string()).into()),
        Value::String(("item".to_string()).into()),
        Value::Array(vec![
            Value::String(("str/concat".to_string()).into()),
            Value::String(("- ".to_string()).into()),
            Value::String(("@item".to_string()).into()),
        ].into()),
    ]);

    let resolved = ctx.resolve_value(&content).into_owned();
    assert_eq!(
        resolved, content,
        "the fn form must pass through literally, not evaluate to a Value::Lambda"
    );
}

fn eval_json(json: &str, ctx: &BindingContext) -> Value {
    let expr: orbital_core_base::schema::sexpr::SExpression = serde_json::from_str(json).unwrap();
    crate::evaluator::Evaluator::new().evaluate(&expr, ctx).unwrap()
}

fn big_list(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| {
                let mut row = ValueMap::default();
                row.insert("n".into(), Value::Number(i as f64));
                row.insert("label".into(), Value::string(format!("row-{i}")));
                Value::object(row)
            })
            .collect(),
    )
}

// L13: a callback's scope is one frame over its parent, so a nested callback never copies an enclosing binding.
#[test]
fn a_nested_callback_does_not_copy_an_enclosing_big_binding() {
    let ctx = BindingContextBuilder::new()
        .local("big", big_list(2000))
        .local("small", Value::Array((0..50).map(|i| Value::Number(i as f64)).collect()))
        .build();
    let started = std::time::Instant::now();
    let out = eval_json(
        r#"["array/map", "@big", ["fn", "x", ["array/reduce", "@small", "@x.n", ["fn", ["acc", "y"], ["+", "@acc", "@y"]]]]]"#,
        &ctx,
    );
    let elapsed = started.elapsed();
    let Value::Array(rows) = out else { panic!("map returns an array") };
    assert_eq!(rows.len(), 2000);
    assert_eq!(rows[1999], Value::Number(1999.0 + 1225.0));
    assert!(elapsed < std::time::Duration::from_secs(2), "nested callbacks took {elapsed:?}");
}

#[test]
fn an_inner_callback_shadows_the_outer_item_and_index_and_sees_outer_params() {
    let ctx = BindingContext::new();
    let out = eval_json(
        r#"["array/map", ["list", 10, 20], ["fn", "x", ["array/map", ["list", 1, 2, 3], ["fn", "y", ["+", ["*", "@x", 100], "@y", ["*", "@index", 1000]]]]]]"#,
        &ctx,
    );
    let expect = |x: f64| Value::Array((0..3).map(|i| Value::Number(x * 100.0 + (i + 1) as f64 + i as f64 * 1000.0)).collect());
    assert_eq!(out, Value::array(vec![expect(10.0), expect(20.0)]));
}

#[test]
fn a_callback_scope_shares_its_enclosing_frames_instead_of_copying_them() {
    let parent = BindingContextBuilder::new().local("big", big_list(2000)).build();
    let mut child = parent.child();
    assert!(Arc::ptr_eq(&child.locals, &parent.locals));
    child.set_local("item", Value::Number(1.0));
    child.set_local("index", Value::Number(0.0));
    assert_eq!(child.locals.vars.len(), 2, "the callback frame holds only its own bindings");
    let shared = child.locals.parent.as_ref().expect("the callback frame sits over its parent's");
    assert!(Arc::ptr_eq(shared, &parent.locals), "the enclosing frame is shared, not copied");
    let mut grandchild = child.child();
    grandchild.set_local("item", Value::Number(2.0));
    assert!(Arc::ptr_eq(grandchild.locals.parent.as_ref().unwrap(), &child.locals));
    assert_eq!(grandchild.resolve("big.length"), Some(Value::Number(2000.0)));
}

#[test]
fn a_child_binding_never_leaks_into_its_parent() {
    let mut parent = BindingContextBuilder::new().local("a", Value::Number(1.0)).build();
    let mut child = parent.child();
    child.set_local("a", Value::Number(2.0));
    child.set_local("b", Value::Number(3.0));
    assert_eq!(parent.resolve("a"), Some(Value::Number(1.0)));
    assert_eq!(parent.resolve("b"), None);
    assert_eq!(child.resolve("a"), Some(Value::Number(2.0)));
    parent.set_local("c", Value::Number(4.0));
    assert_eq!(child.resolve("c"), None, "a child is a snapshot of its parent at creation");
    assert_eq!(child.remove_local("a"), Some(Value::Number(2.0)));
    assert_eq!(child.resolve("a"), None, "a removed local is gone from the child, as with a copied map");
    assert_eq!(parent.resolve("a"), Some(Value::Number(1.0)));
}
