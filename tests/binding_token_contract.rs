use orbital_core::evaluator::{eval_expr, Value};
use orbital_core::fixtures::binding_path_cases;
use orbital_core::schema::SExpression;
use orbital_core::BindingContext;

#[test]
fn binding_tokens_accept_ascii_roots_without_a_name_allowlist() {
    for token in [
        "@entity", "@payload", "@state", "@config", "@user", "@now", "@locale",
        "@currentTheme", "@pages", "@event", "@prevEvents", "@prevStates",
        "@callsitePayload", "@fromState", "@toState", "@trait", "@item", "@response",
        "@Custom_2", "@_", "@9",
    ] {
        assert_eq!(BindingContext::binding_token_end(token, 0), Some(token.len()), "{token}");
    }
}

#[test]
fn binding_tokens_include_ascii_dot_segments_and_bracket_indexes() {
    for token in [
        "@entity.name", "@payload.items[0].name", "@pages[12].href",
        "@payload[000].rows[2].name_9", "@entity.a1._b.0", "@_root[9]._part[10]",
    ] {
        assert_eq!(BindingContext::binding_token_end(token, 0), Some(token.len()), "{token}");
    }
}

#[test]
fn binding_tokens_stop_at_the_exact_grammar_boundary() {
    for (token, suffix) in [
        ("@entity.name", " is ready"),
        ("@entity.name", "-suffix"),
        ("@payload.items[0]", "/next"),
        ("@entity.name", ":value"),
        ("@entity.name", "@payload.value"),
        ("@entity.name", "🙂"),
    ] {
        let input = format!("{token}{suffix}");
        assert_eq!(BindingContext::binding_token_end(&input, 0), Some(token.len()), "{input}");
    }
}

#[test]
fn binding_token_end_is_an_absolute_utf8_byte_position() {
    let prefix = "🪐 زيارة: ";
    let token = "@payload.items[12].name";
    let input = format!("{prefix}{token} — انتهى");
    assert_eq!(
        BindingContext::binding_token_end(&input, prefix.len()),
        Some(prefix.len() + token.len()),
    );
    assert_eq!(BindingContext::binding_token_end(&input, 0), None);
    assert_eq!(BindingContext::binding_token_end("é@entity.name", 1), None);
    assert_eq!(BindingContext::binding_token_end("é@entity.name", 2), Some("é@entity.name".len()));
}

#[test]
fn invalid_starts_and_lone_at_signs_do_not_guess_a_later_token() {
    for input in ["", "@", "@.", "@[0]", "@-name", "@ name", "@\nname", "@@entity.name"] {
        assert_eq!(BindingContext::binding_token_end(input, 0), None, "{input:?}");
    }
    let input = "text @entity.name";
    assert_eq!(BindingContext::binding_token_end(input, 0), None);
    assert_eq!(BindingContext::binding_token_end(input, input.len()), None);
    assert_eq!(BindingContext::binding_token_end(input, input.len() + 1), None);
    assert_eq!(BindingContext::binding_token_end(input, usize::MAX), None);
    assert_eq!(BindingContext::binding_token_end("@@entity.name", 1), Some("@@entity.name".len()));
}

#[test]
fn malformed_brackets_and_dot_segments_leave_the_valid_prefix_end() {
    for (input, prefix) in [
        ("@payload[]", "@payload"),
        ("@payload[-1]", "@payload"),
        ("@payload[+1]", "@payload"),
        ("@payload[1.5]", "@payload"),
        ("@payload[1x]", "@payload"),
        ("@payload[1", "@payload"),
        ("@payload[１２]", "@payload"),
        ("@payload.items[", "@payload.items"),
        ("@payload.items[2].", "@payload.items[2]"),
        ("@payload..name", "@payload"),
        ("@payload.[0]", "@payload"),
        ("@payload[0]name", "@payload[0]"),
    ] {
        assert_eq!(BindingContext::binding_token_end(input, 0), Some(prefix.len()), "{input}");
    }
}

#[test]
fn binding_word_characters_are_ascii_even_in_unicode_text() {
    for input in ["@é", "@名字", "@١"] {
        assert_eq!(BindingContext::binding_token_end(input, 0), None, "{input}");
    }
    for (input, prefix) in [
        ("@payload.é", "@payload"),
        ("@payload.naïve", "@payload.na"),
        ("@payload.x١", "@payload.x"),
        ("@payload.名字", "@payload"),
    ] {
        assert_eq!(BindingContext::binding_token_end(input, 0), Some(prefix.len()), "{input}");
    }
}

// EXEC-EVAL-6: the shared `fixtures/binding-path/cases.json` (dotted number indexes, bracket is a literal key).

#[derive(serde::Deserialize)]
struct PathCase {
    name: String,
    binding: String,
    found: bool,
    value: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct PathFixture {
    payload: serde_json::Value,
    cases: Vec<PathCase>,
}

fn path_fixture() -> PathFixture {
    serde_json::from_str(binding_path_cases()).expect("binding-path fixture parses")
}

fn path_ctx(f: &PathFixture) -> BindingContext {
    let mut c = BindingContext::new();
    let payload: Value = serde_json::from_value(f.payload.clone()).expect("payload value");
    c.set_payload(payload);
    c
}

#[test]
fn binding_path_every_case_resolves_as_the_fixture_says() {
    let f = path_fixture();
    let c = path_ctx(&f);
    for case in &f.cases {
        let got = c.resolve_binding(&case.binding);
        if case.found {
            let want: Value = serde_json::from_value(case.value.clone().expect("found has a value")).unwrap();
            assert_eq!(got, Some(want), "resolve `{}`", case.name);
        } else {
            assert_eq!(got, None, "resolve `{}`", case.name);
        }
    }
}

#[test]
fn binding_path_every_case_evaluates_as_the_fixture_says() {
    let f = path_fixture();
    let c = path_ctx(&f);
    for case in &f.cases {
        let expr = SExpression::String(case.binding.clone());
        let got = eval_expr(&expr, &c).unwrap();
        let want = if case.found {
            serde_json::from_value(case.value.clone().unwrap()).unwrap()
        } else {
            Value::Null
        };
        assert_eq!(got, want, "eval `{}`", case.name);
    }
}
