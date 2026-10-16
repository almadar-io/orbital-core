//! `time/*` unit semantics, and the parity contract with the JS interpreter.
//!
//! `S-CALENDAR-RANGE-FILTER-USES-UNSUPPORTED-TIME-UNITS`: `std-calendar.lolo`
//! built its date-range filter from `(time/subtract (time/now) 3 month)`, which
//! the JS path evaluated happily and this path rejected as an unknown unit — so
//! rows were dropped fail-closed on the compiled path while the run reported
//! PASS. Auditing the rest of the `time/*` family turned up four more silent
//! divergences: `time/diff` returned `.abs()` here and a signed value there,
//! and fell back to raw milliseconds for `month`/`year`; `time/startOf` began
//! weeks on Monday here and Sunday there; `time/isSame` degraded to an exact
//! millisecond comparison for any unit it did not name; and an unknown unit was
//! a hard error here but a silent no-op there.
//!
//! The case table below is the contract. `packages/almadar-evaluator/__tests__/
//! time-unit-parity.test.ts` runs the identical table through the JS operators
//! and asserts the identical expectations, so a change to one path that is not
//! mirrored in the other fails a test rather than shipping.

use orbital_core_eval::evaluator::{BindingContext, Evaluator};
use orbital_core_base::schema::SExpression;
use orbital_core_base::value::Value;

/// 2024-03-31T12:34:56.789Z — a leap year, on the 31st, so month arithmetic has
/// to clamp, and with a time-of-day that must survive the shift.
const MAR_31_2024: f64 = 1_711_888_496_789.0;
/// 2024-01-15T00:00:00.000Z
const JAN_15_2024: f64 = 1_705_276_800_000.0;

fn eval(json: &str) -> Value {
    let expr: SExpression = serde_json::from_str(json).unwrap();
    Evaluator::new()
        .evaluate(&expr, &BindingContext::new())
        .unwrap()
}

fn eval_err(json: &str) -> String {
    let expr: SExpression = serde_json::from_str(json).unwrap();
    Evaluator::new()
        .evaluate(&expr, &BindingContext::new())
        .unwrap_err()
        .to_string()
}

fn num(json: &str) -> f64 {
    match eval(json) {
        Value::Number(n) => n,
        other => panic!("expected number from {json}, got {other:?}"),
    }
}

fn boolean(json: &str) -> bool {
    match eval(json) {
        Value::Bool(b) => b,
        other => panic!("expected bool from {json}, got {other:?}"),
    }
}

/// The regression this whole change exists for: the four branches of
/// `std-calendar`'s range filter must all evaluate.
#[test]
fn calendar_range_filter_units_all_evaluate() {
    for (amount, unit) in [(1, "week"), (3, "month"), (1, "year"), (1, "month")] {
        let expr = format!(r#"["time/subtract", {MAR_31_2024}, {amount}, "{unit}"]"#);
        let got = num(&expr);
        assert!(
            got < MAR_31_2024,
            "{amount} {unit} before 2024-03-31 should be earlier, got {got}"
        );
    }
}

#[test]
fn month_arithmetic_is_calendar_correct_and_clamps() {
    // 31 Mar − 1 month = 29 Feb (2024 is a leap year), NOT 2 Mar via overflow.
    // 2024-02-29T12:34:56.789Z
    assert_eq!(
        num(&format!(r#"["time/subtract", {MAR_31_2024}, 1, "month"]"#)),
        1_709_210_096_789.0
    );
    // 31 Mar − 3 months = 31 Dec 2023 (December has 31 days, so no clamp).
    assert_eq!(
        num(&format!(r#"["time/subtract", {MAR_31_2024}, 3, "month"]"#)),
        1_704_026_096_789.0
    );
    // 31 Mar 2024 − 1 year = 31 Mar 2023.
    assert_eq!(
        num(&format!(r#"["time/subtract", {MAR_31_2024}, 1, "year"]"#)),
        1_680_266_096_789.0
    );
    // Adding is the same operation with the opposite sign.
    assert_eq!(
        num(&format!(r#"["time/add", {MAR_31_2024}, 11, "month"]"#)),
        num(&format!(
            r#"["time/subtract", {MAR_31_2024}, -11, "month"]"#
        ))
    );
}

#[test]
fn clamping_is_not_reversible_and_that_is_correct() {
    // 31 Mar − 1 month = 29 Feb, and 29 Feb + 1 month = 29 Mar. Clamping loses
    // the day-of-month; asserted so nobody "fixes" it into overflow later.
    let back = num(&format!(
        r#"["time/add", ["time/subtract", {MAR_31_2024}, 1, "month"], 1, "month"]"#
    ));
    assert_ne!(back, MAR_31_2024);
    assert_eq!(back, 1_711_715_696_789.0); // 2024-03-29T12:34:56.789Z
}

#[test]
fn every_accepted_unit_spelling_resolves() {
    let day = num(&format!(r#"["time/subtract", {JAN_15_2024}, 1, "day"]"#));
    for spelling in ["d", "days"] {
        assert_eq!(
            num(&format!(
                r#"["time/subtract", {JAN_15_2024}, 1, "{spelling}"]"#
            )),
            day,
            "{spelling} must be the same unit as day"
        );
    }
    // `m` is minute, not month — the abbreviation table's one real trap.
    assert_eq!(
        num(&format!(r#"["time/subtract", {JAN_15_2024}, 1, "m"]"#)),
        num(&format!(r#"["time/subtract", {JAN_15_2024}, 1, "minute"]"#))
    );
    assert_ne!(
        num(&format!(r#"["time/subtract", {JAN_15_2024}, 1, "m"]"#)),
        num(&format!(r#"["time/subtract", {JAN_15_2024}, 1, "month"]"#))
    );
}

#[test]
fn unknown_unit_is_a_hard_error_everywhere() {
    for op in ["time/add", "time/subtract"] {
        let msg = eval_err(&format!(r#"["{op}", {JAN_15_2024}, 1, "fortnight"]"#));
        assert!(
            msg.contains("fortnight"),
            "{op} must name the bad unit: {msg}"
        );
    }
    for op in ["time/startOf", "time/endOf"] {
        let msg = eval_err(&format!(r#"["{op}", {JAN_15_2024}, "fortnight"]"#));
        assert!(
            msg.contains("fortnight"),
            "{op} must name the bad unit: {msg}"
        );
    }
    let msg = eval_err(&format!(
        r#"["time/diff", {MAR_31_2024}, {JAN_15_2024}, "fortnight"]"#
    ));
    assert!(msg.contains("fortnight"), "time/diff must reject it: {msg}");
    let msg = eval_err(&format!(
        r#"["time/isSame", {MAR_31_2024}, {JAN_15_2024}, "fortnight"]"#
    ));
    assert!(
        msg.contains("fortnight"),
        "time/isSame must reject it: {msg}"
    );
}

#[test]
fn diff_is_signed_and_calendar_correct() {
    // Signed: diff(a, b) == -diff(b, a). It used to return .abs() here.
    assert_eq!(
        num(&format!(
            r#"["time/diff", {MAR_31_2024}, {JAN_15_2024}, "day"]"#
        )),
        76.0
    );
    assert_eq!(
        num(&format!(
            r#"["time/diff", {JAN_15_2024}, {MAR_31_2024}, "day"]"#
        )),
        -76.0
    );
    // Whole calendar months, not a 30.44-day approximation: 15 Jan → 31 Mar is
    // two whole months (15 Mar) plus a remainder.
    assert_eq!(
        num(&format!(
            r#"["time/diff", {MAR_31_2024}, {JAN_15_2024}, "month"]"#
        )),
        2.0
    );
    assert_eq!(
        num(&format!(
            r#"["time/diff", {MAR_31_2024}, {JAN_15_2024}, "year"]"#
        )),
        0.0
    );
    // A partial trailing month does not count: 31 Jan → 28 Feb is 0 months.
    // 2024-01-31T00:00:00Z / 2024-02-28T00:00:00Z
    assert_eq!(
        num(r#"["time/diff", 1709078400000, 1706659200000, "month"]"#),
        0.0
    );
}

#[test]
fn start_of_week_is_monday_and_end_of_is_the_last_millisecond() {
    // 2024-01-15 is a Monday, so startOf week is that midnight exactly.
    assert_eq!(
        num(&format!(r#"["time/startOf", {JAN_15_2024}, "week"]"#)),
        JAN_15_2024
    );
    // endOf(unit) is startOf(next unit) − 1, for every unit.
    for unit in ["minute", "hour", "day", "week", "month", "year"] {
        let start = num(&format!(r#"["time/startOf", {MAR_31_2024}, "{unit}"]"#));
        let end = num(&format!(r#"["time/endOf", {MAR_31_2024}, "{unit}"]"#));
        assert!(
            start <= MAR_31_2024 && MAR_31_2024 <= end,
            "{unit} bucket must contain the timestamp"
        );
        assert_eq!(
            num(&format!(r#"["time/startOf", {end}, "{unit}"]"#)),
            start,
            "endOf {unit} must stay inside the same bucket"
        );
    }
}

#[test]
fn is_same_uses_the_unit_bucket_not_an_exact_match() {
    // Both in March 2024, 30 days apart — same month, different day. This
    // returned false before, because `month` fell through to an exact compare.
    // 2024-03-01T00:00:00Z
    let mar_1 = 1_709_251_200_000.0_f64;
    assert!(boolean(&format!(
        r#"["time/isSame", {MAR_31_2024}, {mar_1}, "month"]"#
    )));
    assert!(boolean(&format!(
        r#"["time/isSame", {MAR_31_2024}, {mar_1}, "year"]"#
    )));
    assert!(!boolean(&format!(
        r#"["time/isSame", {MAR_31_2024}, {mar_1}, "day"]"#
    )));
    assert!(boolean(&format!(
        r#"["time/isSame", {JAN_15_2024}, {MAR_31_2024}, "year"]"#
    )));
}

/// `quarter`: 3-month calendar buckets. Identical table in the JS twin
/// (`time-unit-parity.test.ts`); std-graphs' `period: quarter` knob needs it.
#[test]
fn quarter_is_a_calendar_unit() {
    const Q1_2024: f64 = 1_704_067_200_000.0;
    const JUN_30_2024_SAME_TIME: f64 = 1_719_750_896_789.0;
    const MAR_31_2024_LAST_MS: f64 = 1_711_929_599_999.0;
    const APR_1_2024_SAME_TIME: f64 = 1_711_974_896_789.0;
    assert_eq!(num(&format!(r#"["time/startOf", {MAR_31_2024}, "quarter"]"#)), Q1_2024);
    assert_eq!(num(&format!(r#"["time/endOf", {MAR_31_2024}, "quarter"]"#)), MAR_31_2024_LAST_MS);
    assert_eq!(num(&format!(r#"["time/add", {MAR_31_2024}, 1, "quarter"]"#)), JUN_30_2024_SAME_TIME);
    // Whole quarters, like whole months: 15 Jan → 15 Apr is one; 31 Mar → 30 Jun (clamped) is not.
    assert_eq!(num(&format!(r#"["time/diff", 1713139200000, {JAN_15_2024}, "quarter"]"#)), 1.0);
    assert_eq!(num(&format!(r#"["time/diff", {JUN_30_2024_SAME_TIME}, {MAR_31_2024}, "quarter"]"#)), 0.0);
    assert!(boolean(&format!(r#"["time/isSame", {JAN_15_2024}, {MAR_31_2024}, "quarter"]"#)));
    assert!(!boolean(&format!(r#"["time/isSame", {MAR_31_2024}, {APR_1_2024_SAME_TIME}, "quarter"]"#)));
}

