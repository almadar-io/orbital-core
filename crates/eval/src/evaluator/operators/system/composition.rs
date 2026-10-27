//! Composition (behavior) operators.

use super::*;

// ============================================================================
// Composition (Behavior) - parity placeholder
// ============================================================================
//
// `behavior/compose`, `behavior/wire`, `behavior/detect-layout`, and
// `behavior/pipe` are compile-time composition operators. They are resolved
// natively by the orbital-compiler `Compose` phase (Phase 0 sub-unit 0.3) and
// are never expected to reach the runtime evaluator. The stub below exists
// only to keep `operators.json` ↔ `operators.rs` parity green; if it is ever
// invoked at runtime, it raises a clear error so the compiler bug is obvious.
pub struct BehaviorCompositionStubOp(pub &'static str);
impl Operator for BehaviorCompositionStubOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "'{}' is a compile-time composition operator and must be resolved by the orbital-compiler Compose phase, not evaluated at runtime",
            self.0
        ))))
    }
}

/// Unix-ms for a Y/M/D at 00:00:00 UTC, using this file's no-chrono date math.
pub(super) fn date_to_timestamp(year: i32, month: i32, day: i32) -> f64 {
    let month_days: [i64; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut days = days_since_epoch(year);
    for days_in_month in month_days
        .iter()
        .take(((month as usize).saturating_sub(1)).min(11))
    {
        days += days_in_month;
    }
    days += (day - 1) as i64;
    (days as f64) * 24.0 * 60.0 * 60.0 * 1000.0
}

/// Start of the unit-bucket containing `ts`. The single implementation behind
/// `time/startOf` and `time/isSame`. Weeks start **Monday** (ISO-8601).
pub(super) fn start_of(ts: f64, unit: TimeUnit) -> f64 {
    let day_ms = 24.0 * 60.0 * 60.0 * 1000.0;
    match unit {
        TimeUnit::Millisecond => ts.floor(),
        TimeUnit::Second => (ts / 1000.0).floor() * 1000.0,
        TimeUnit::Minute => (ts / 60_000.0).floor() * 60_000.0,
        TimeUnit::Hour => (ts / 3_600_000.0).floor() * 3_600_000.0,
        TimeUnit::Day => (ts / day_ms).floor() * day_ms,
        TimeUnit::Week => {
            let days = (ts / day_ms).floor() as i64;
            let dow = (days + 3).rem_euclid(7); // Jan 1 1970 = Thu; 0 => Monday
            ((days - dow) as f64) * day_ms
        }
        TimeUnit::Month => {
            let (y, m, _) = timestamp_to_date(ts);
            date_to_timestamp(y, m, 1)
        }
        TimeUnit::Quarter => {
            let (y, m, _) = timestamp_to_date(ts);
            date_to_timestamp(y, (m - 1) / 3 * 3 + 1, 1)
        }
        TimeUnit::Year => {
            let (y, _, _) = timestamp_to_date(ts);
            date_to_timestamp(y, 1, 1)
        }
    }
}

/// time/startOf - start of a time period.
pub struct TimeStartOfOp;
impl Operator for TimeStartOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/startOf", >= 2);
        let ts = eval_expr(&args[0], ctx)?.to_number().unwrap_or(0.0);
        let unit = TimeUnit::parse(&eval_expr(&args[1], ctx)?.to_string_value())?;
        Ok(Value::Number(start_of(ts, unit)))
    }
}

/// time/endOf - last millisecond of a time period.
pub struct TimeEndOfOp;
impl Operator for TimeEndOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/endOf", >= 2);
        let ts = eval_expr(&args[0], ctx)?.to_number().unwrap_or(0.0);
        let unit = TimeUnit::parse(&eval_expr(&args[1], ctx)?.to_string_value())?;
        let day_ms = 24.0 * 60.0 * 60.0 * 1000.0;
        let result = match unit {
            TimeUnit::Millisecond => ts.floor(),
            TimeUnit::Second => ((ts / 1000.0).floor() + 1.0) * 1000.0 - 1.0,
            TimeUnit::Minute => ((ts / 60_000.0).floor() + 1.0) * 60_000.0 - 1.0,
            TimeUnit::Hour => ((ts / 3_600_000.0).floor() + 1.0) * 3_600_000.0 - 1.0,
            TimeUnit::Day => ((ts / day_ms).floor() + 1.0) * day_ms - 1.0,
            TimeUnit::Week => {
                let days = (ts / day_ms).floor() as i64;
                let dow = (days + 3).rem_euclid(7);
                (((days - dow) + 7) as f64) * day_ms - 1.0
            }
            TimeUnit::Month => {
                let (y, m, _) = timestamp_to_date(ts);
                let (ny, nm) = if m >= 12 { (y + 1, 1) } else { (y, m + 1) };
                date_to_timestamp(ny, nm, 1) - 1.0
            }
            TimeUnit::Quarter => {
                let (y, m, _) = timestamp_to_date(ts);
                let next = (m - 1) / 3 * 3 + 4;
                let (ny, nm) = if next > 12 {
                    (y + 1, next - 12)
                } else {
                    (y, next)
                };
                date_to_timestamp(ny, nm, 1) - 1.0
            }
            TimeUnit::Year => {
                let (y, _, _) = timestamp_to_date(ts);
                date_to_timestamp(y + 1, 1, 1) - 1.0
            }
        };
        Ok(Value::Number(result))
    }
}

/// time/relative - human relative time ("2 hours ago", "in 3 days").
pub struct TimeRelativeOp;
impl Operator for TimeRelativeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/relative", 1);
        let ts = eval_expr(&args[0], ctx)?.to_number().unwrap_or(0.0);
        let now = ctx.now_millis();
        let diff = now - ts; // >0 past, <0 future
        let plural = |n: i64| if n == 1 { "" } else { "s" };
        let phrase = |n: i64, unit: &str| {
            if diff < 0.0 {
                format!("in {} {}{}", n, unit, plural(n))
            } else {
                format!("{} {}{} ago", n, unit, plural(n))
            }
        };
        // Same buckets as the runtime evaluator (`evalTimeRelative`) and the compiled TS emit.
        let ms = diff.abs();
        let (minute, hour, day) = (60_000.0, 3_600_000.0, 86_400_000.0);
        let out = if ms < minute {
            "just now".to_string()
        } else if ms < hour {
            phrase((ms / minute).round() as i64, "minute")
        } else if ms < day {
            phrase((ms / hour).round() as i64, "hour")
        } else if ms < 7.0 * day {
            phrase((ms / day).round() as i64, "day")
        } else if ms < 30.0 * day {
            phrase((ms / (7.0 * day)).round() as i64, "week")
        } else if ms < 365.0 * day {
            phrase((ms / (30.0 * day)).round() as i64, "month")
        } else {
            phrase((ms / (365.0 * day)).round() as i64, "year")
        };
        Ok(Value::string(out))
    }
}

/// format/creditCard - mask all but the last 4 digits, grouped in 4s.
pub struct FormatCreditCardOp;
impl Operator for FormatCreditCardOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/creditCard", 1);
        let digits: String = eval_expr(&args[0], ctx)?
            .to_string_value()
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        if digits.len() <= 4 {
            return Ok(Value::string(digits));
        }
        let last4 = &digits[digits.len() - 4..];
        let mut groups: Vec<String> = Vec::new();
        let mask_len = digits.len() - 4;
        let mut i = 0;
        while i < mask_len {
            let g = (mask_len - i).min(4);
            groups.push("•".repeat(g));
            i += g;
        }
        groups.push(last4.to_string());
        Ok(Value::string(groups.join(" ")))
    }
}

