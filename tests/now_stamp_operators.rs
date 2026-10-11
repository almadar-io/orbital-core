// Every time operator reads the context's `now` stamp, never the wall clock; twin of almadar-evaluator `now-stamp.test.ts`.

use orbital_core::{BindingContextBuilder, Evaluator, Value};

const STAMP: f64 = 1_592_222_400_000.0; // 2020-06-15T12:00:00Z
const DAY: f64 = 86_400_000.0;

fn eval_at(now: f64, expr: serde_json::Value) -> Value {
    let ctx = BindingContextBuilder::new().now(now).build();
    let sexpr = serde_json::from_value(expr).unwrap();
    Evaluator::new().evaluate(&sexpr, &ctx).unwrap()
}

#[test]
fn now_binding_and_time_now_return_the_stamp() {
    assert_eq!(
        eval_at(STAMP, serde_json::json!("@now")),
        Value::Number(STAMP)
    );
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/now"])),
        Value::Number(STAMP)
    );
}

#[test]
fn a_lambda_child_context_inherits_the_stamp() {
    let out = eval_at(
        STAMP,
        serde_json::json!(["array/map", ["list", 1, 2, 3], ["fn", "x", ["time/now"]]]),
    );
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Number(STAMP),
            Value::Number(STAMP),
            Value::Number(STAMP)
        ])
    );
}

#[test]
fn is_past_and_is_future_compare_against_the_stamp() {
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/isFuture", STAMP + DAY])),
        Value::Bool(true)
    );
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/isPast", STAMP + DAY])),
        Value::Bool(false)
    );
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/isPast", STAMP - 1.0])),
        Value::Bool(true)
    );
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/isPast", STAMP])),
        Value::Bool(false)
    );
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/isFuture", STAMP])),
        Value::Bool(false)
    );
}

#[test]
fn is_today_and_today_are_relative_to_the_stamp_day() {
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/isToday", STAMP + 60_000.0])),
        Value::Bool(true)
    );
    assert_eq!(
        eval_at(
            STAMP,
            serde_json::json!(["time/isToday", STAMP + 2.0 * DAY])
        ),
        Value::Bool(false)
    );
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/today"])),
        Value::Number((STAMP / DAY).floor() * DAY)
    );
}

#[test]
fn relative_formatting_measures_from_the_stamp() {
    assert_eq!(
        eval_at(STAMP, serde_json::json!(["time/relative", STAMP])),
        Value::String("just now".into())
    );
    let three_days = eval_at(
        STAMP,
        serde_json::json!(["time/relative", STAMP - 3.0 * DAY]),
    );
    assert!(
        matches!(&three_days, Value::String(s) if s.contains("3 days ago")),
        "{three_days:?}"
    );
}

#[test]
fn an_unstamped_context_reads_the_wall_clock() {
    let ctx = BindingContextBuilder::new().build();
    let sexpr = serde_json::from_value(serde_json::json!(["time/now"])).unwrap();
    let Value::Number(n) = Evaluator::new().evaluate(&sexpr, &ctx).unwrap() else {
        panic!("number")
    };
    assert!(n > STAMP + 365.0 * DAY);
}

/// One definition on every path (the runtime evaluator's `evalTimeRelative`): under a minute is
/// "just now", then minutes, hours, days, weeks, 30-day months and 365-day years, rounded.
#[test]
fn relative_formatting_uses_the_runtime_buckets() {
    let rel = |offset: f64| eval_at(STAMP, serde_json::json!(["time/relative", STAMP + offset]));
    let s = |text: &str| Value::String(text.into());
    assert_eq!(rel(-30_000.0), s("just now"));
    assert_eq!(rel(-45.0 * 60_000.0), s("45 minutes ago"));
    assert_eq!(rel(-60_000.0), s("1 minute ago"));
    assert_eq!(rel(-2.0 * 3_600_000.0), s("2 hours ago"));
    assert_eq!(rel(-3.0 * DAY), s("3 days ago"));
    assert_eq!(rel(-14.0 * DAY), s("2 weeks ago"));
    assert_eq!(rel(-60.0 * DAY), s("2 months ago"));
    assert_eq!(rel(-730.0 * DAY), s("2 years ago"));
    assert_eq!(rel(3.0 * DAY), s("in 3 days"));
    assert_eq!(rel(DAY), s("in 1 day"));
}
