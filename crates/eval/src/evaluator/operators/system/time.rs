//! Time operators over ISO-8601 text: a `date` is `YYYY-MM-DD` (a calendar day, no zone), a `datetime` or `instant` is a full date-time with a zone.

use super::*;

/// The one time-unit vocabulary. Every `time/*` operator resolves its unit
/// argument through `TimeUnit::parse`, and the JS interpreter's
/// `packages/almadar-evaluator/std/time.ts` mirrors this table exactly — the two
/// execution paths diverged silently once already
/// (`S-CALENDAR-RANGE-FILTER-USES-UNSUPPORTED-TIME-UNITS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeUnit {
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

/// Every spelling this vocabulary accepts, in one place so the error message
/// and the parser can never drift apart.
pub const TIME_UNIT_VOCABULARY: &str =
    "ms|millisecond(s), s|second(s), m|minute(s), h|hour(s), d|day(s), w|week(s), month(s), quarter(s), year(s)";

impl TimeUnit {
    pub fn parse(unit: &str) -> Result<Self, OrbitalError> {
        match unit {
            "ms" | "millisecond" | "milliseconds" => Ok(TimeUnit::Millisecond),
            "s" | "second" | "seconds" => Ok(TimeUnit::Second),
            "m" | "minute" | "minutes" => Ok(TimeUnit::Minute),
            "h" | "hour" | "hours" => Ok(TimeUnit::Hour),
            "d" | "day" | "days" => Ok(TimeUnit::Day),
            "w" | "week" | "weeks" => Ok(TimeUnit::Week),
            "month" | "months" => Ok(TimeUnit::Month),
            "quarter" | "quarters" => Ok(TimeUnit::Quarter),
            "year" | "years" => Ok(TimeUnit::Year),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: format!("time unit ({TIME_UNIT_VOCABULARY})"),
                actual: unit.to_string(),
            })),
        }
    }

    /// Fixed-length units expressed in milliseconds. `Month`/`Quarter`/`Year` have no
    /// fixed length and return `None` — callers must use calendar arithmetic.
    pub fn fixed_millis(self) -> Option<f64> {
        Some(match self {
            TimeUnit::Millisecond => 1.0,
            TimeUnit::Second => 1000.0,
            TimeUnit::Minute => 60.0 * 1000.0,
            TimeUnit::Hour => 60.0 * 60.0 * 1000.0,
            TimeUnit::Day => 24.0 * 60.0 * 60.0 * 1000.0,
            TimeUnit::Week => 7.0 * 24.0 * 60.0 * 60.0 * 1000.0,
            TimeUnit::Month | TimeUnit::Quarter | TimeUnit::Year => return None,
        })
    }
}

pub(super) fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 30,
    }
}

/// Calendar-correct month shift. The day-of-month **clamps** into the target
/// month (31 Mar − 1 month = 28/29 Feb) rather than overflowing into the next
/// one; time-of-day is preserved. The JS path clamps identically.
pub(super) fn add_calendar_months(ts: f64, months: i64) -> f64 {
    let (y, m, d) = timestamp_to_date(ts);
    let time_of_day = ts - date_to_timestamp(y, m, d);
    let total = (y as i64) * 12 + (m as i64 - 1) + months;
    let ny = total.div_euclid(12) as i32;
    let nm = total.rem_euclid(12) as i32 + 1;
    date_to_timestamp(ny, nm, d.min(days_in_month(ny, nm))) + time_of_day
}

/// Signed count of **whole** calendar months from `from` to `to`. A partial
/// trailing month does not count, so 31 Jan → 28 Feb is 0 months, matching the
/// clamping in `add_calendar_months`.
pub(super) fn whole_months_between(from: f64, to: f64) -> i64 {
    let (lo, hi) = (from.min(to), from.max(to));
    let (y1, m1, d1) = timestamp_to_date(lo);
    let (y2, m2, d2) = timestamp_to_date(hi);
    let tod1 = lo - date_to_timestamp(y1, m1, d1);
    let tod2 = hi - date_to_timestamp(y2, m2, d2);

    let mut months = ((y2 - y1) as i64) * 12 + (m2 - m1) as i64;
    if (d2, tod2) < (d1, tod1) {
        months -= 1;
    }
    if to < from {
        -months
    } else {
        months
    }
}

/// Shift `ts` by `amount` of `unit`. The single implementation behind both
/// `time/add` and `time/subtract`.
pub(super) fn shift_time(ts: f64, amount: f64, unit: TimeUnit) -> Result<f64, OrbitalError> {
    match unit.fixed_millis() {
        Some(ms) => Ok(ts + amount * ms),
        None => {
            if amount.fract() != 0.0 || amount.abs() > f64::from(u32::MAX) {
                return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "a calendar unit takes a whole amount, got {amount}"
                ))));
            }
            let months = match unit {
                TimeUnit::Year => amount as i64 * 12,
                TimeUnit::Quarter => amount as i64 * 3,
                _ => amount as i64,
            };
            Ok(add_calendar_months(ts, months))
        }
    }
}

/// Whether a point in time names a calendar day or a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Date,
    DateTime,
}

/// A parsed ISO-8601 value: epoch milliseconds (a date is its midnight UTC) and the shape it was written in.
#[derive(Debug, Clone, Copy)]
pub(super) struct Moment {
    pub ms: f64,
    pub shape: Shape,
}

impl Moment {
    pub(super) fn of(value: &Value) -> Result<Moment, OrbitalError> {
        match value {
            Value::String(text) => parse_iso(text)
                .map(|(ms, shape)| Moment {
                    ms: ms as f64,
                    shape,
                })
                .ok_or_else(|| {
                    OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: "date or datetime (ISO-8601 text)".to_string(),
                        actual: format!("\"{text}\""),
                    })
                }),
            other => Err(OrbitalError::Eval(
                other.kind_error("date or datetime (ISO-8601 text)"),
            )),
        }
    }

    /// The milliseconds of a moment that carries a time of day; a bare date refuses.
    pub(super) fn at_time_of_day(self, op: &str) -> Result<f64, OrbitalError> {
        match self.shape {
            Shape::DateTime => Ok(self.ms),
            Shape::Date => Err(needs_time_of_day(op)),
        }
    }
}

pub(super) fn needs_time_of_day(op: &str) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(format!(
        "{op} needs a time of day and a date carries none: pass a datetime"
    )))
}

impl TimeUnit {
    pub(super) fn below_a_day(self) -> bool {
        matches!(
            self,
            TimeUnit::Millisecond | TimeUnit::Second | TimeUnit::Minute | TimeUnit::Hour
        )
    }
}

fn utc(ms: f64) -> Result<chrono::DateTime<chrono::Utc>, OrbitalError> {
    if ms.is_finite() {
        if let Some(at) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms.floor() as i64)
        {
            use chrono::Datelike;
            if (0..=9999).contains(&at.year()) {
                return Ok(at);
            }
        }
    }
    Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
        "{ms} ms is outside the years 0000 to 9999"
    ))))
}

/// `YYYY-MM-DD` for a date, `YYYY-MM-DDTHH:MM:SS.mmmZ` for a datetime or instant.
pub(super) fn render_moment(ms: f64, shape: Shape) -> Result<Value, OrbitalError> {
    use chrono::{Datelike, Timelike};
    let at = utc(ms)?;
    Ok(Value::string(match shape {
        Shape::Date => format!("{:04}-{:02}-{:02}", at.year(), at.month(), at.day()),
        Shape::DateTime => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            at.year(),
            at.month(),
            at.day(),
            at.hour(),
            at.minute(),
            at.second(),
            at.timestamp_subsec_millis()
        ),
    }))
}

/// An epoch-ms clock reading as an ISO-8601 instant.
pub(crate) fn instant_value(ms: f64) -> Result<Value, OrbitalError> {
    render_moment(ms, Shape::DateTime)
}

/// time/today - midnight UTC today, as an instant
pub struct TimeTodayOp;
impl Operator for TimeTodayOp {
    fn evaluate(&self, _args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let day = 24.0 * 60.0 * 60.0 * 1000.0;
        render_moment((ctx.now_millis() / day).floor() * day, Shape::DateTime)
    }
}

pub struct TimeNowOp;
impl Operator for TimeNowOp {
    fn evaluate(&self, _args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        render_moment(ctx.now_millis(), Shape::DateTime)
    }
}

fn unit_arg(arg: &SExpression, ctx: &BindingContext) -> Result<TimeUnit, OrbitalError> {
    TimeUnit::parse(eval_expr(arg, ctx)?.text()?)
}

/// Shift a moment, keeping its shape. A date shifts by whole days or longer only.
fn shift_moment(m: Moment, amount: f64, unit: TimeUnit, op: &str) -> EvalResult {
    if m.shape == Shape::Date {
        if unit.below_a_day() {
            return Err(needs_time_of_day(op));
        }
        if unit.fixed_millis().is_some() && amount.fract() != 0.0 {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "{op} on a date takes a whole amount of days or weeks, got {amount}"
            ))));
        }
    }
    render_moment(shift_time(m.ms, amount, unit)?, m.shape)
}

/// time/add - add a duration to a date or datetime
pub struct TimeAddOp;
impl Operator for TimeAddOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/add", >= 3);
        let m = Moment::of(&eval_expr(&args[0], ctx)?)?;
        let amount = require_number!(eval_expr(&args[1], ctx)?);
        shift_moment(m, amount, unit_arg(&args[2], ctx)?, "time/add")
    }
}

/// time/subtract - subtract a duration from a date or datetime
pub struct TimeSubtractOp;
impl Operator for TimeSubtractOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/subtract", >= 3);
        let m = Moment::of(&eval_expr(&args[0], ctx)?)?;
        let amount = require_number!(eval_expr(&args[1], ctx)?);
        shift_moment(m, -amount, unit_arg(&args[2], ctx)?, "time/subtract")
    }
}

fn same_shape(a: Moment, b: Moment, op: &str) -> Result<(), OrbitalError> {
    if a.shape == b.shape {
        return Ok(());
    }
    Err(OrbitalError::Eval(EvalError::TypeMismatch {
        expected: "two dates or two datetimes".to_string(),
        actual: format!("{op} got a date and a datetime: convert one explicitly"),
    }))
}

fn pair(
    args: &[SExpression],
    ctx: &BindingContext,
    op: &str,
) -> Result<(Moment, Moment), OrbitalError> {
    let (a, b) = (
        Moment::of(&eval_expr(&args[0], ctx)?)?,
        Moment::of(&eval_expr(&args[1], ctx)?)?,
    );
    same_shape(a, b, op)?;
    Ok((a, b))
}

/// time/diff - signed difference in ms, or in whole units of the third argument
pub struct TimeDiffOp;
impl Operator for TimeDiffOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/diff", >= 2);
        let (a, b) = pair(args, ctx, "time/diff")?;
        let (ts1, ts2) = (a.ms, b.ms);
        let diff_ms = ts1 - ts2;

        if args.len() > 2 {
            let unit = unit_arg(&args[2], ctx)?;
            let result = match unit.fixed_millis() {
                Some(ms) => (diff_ms / ms).trunc(),
                None => {
                    let months = whole_months_between(ts2, ts1) as f64;
                    match unit {
                        TimeUnit::Year => (months / 12.0).trunc(),
                        TimeUnit::Quarter => (months / 3.0).trunc(),
                        _ => months,
                    }
                }
            };
            Ok(Value::Number(result))
        } else {
            Ok(Value::Number(diff_ms))
        }
    }
}

/// time/isBefore
pub struct TimeIsBeforeOp;
impl Operator for TimeIsBeforeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isBefore", >= 2);
        let (a, b) = pair(args, ctx, "time/isBefore")?;
        Ok(Value::Bool(a.ms < b.ms))
    }
}

/// time/isAfter
pub struct TimeIsAfterOp;
impl Operator for TimeIsAfterOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isAfter", >= 2);
        let (a, b) = pair(args, ctx, "time/isAfter")?;
        Ok(Value::Bool(a.ms > b.ms))
    }
}

/// `YYYY-MM-DD` (a date) or `YYYY-MM-DDTHH:MM[:SS[.f+]]` with a `Z` or `±HH:MM` zone (a datetime) to epoch ms and shape.
/// `None` for every other spelling, a date-time without a zone, or an out-of-range part.
pub fn parse_iso(text: &str) -> Option<(i64, Shape)> {
    use chrono::{NaiveDate, TimeZone};
    let b = text.as_bytes();
    let num = |from: usize, len: usize| -> Option<u32> {
        let slice = b.get(from..from + len)?;
        if slice.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(slice).ok()?.parse().ok()
        } else {
            None
        }
    };
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(num(0, 4)? as i32, num(5, 2)?, num(8, 2)?)?;
    let mut ms = chrono::Utc
        .from_utc_datetime(&date.and_hms_opt(0, 0, 0)?)
        .timestamp_millis();
    if b.len() == 10 {
        return Some((ms, Shape::Date));
    }
    if b.get(10) != Some(&b'T') || b.get(13) != Some(&b':') {
        return None;
    }
    let (hour, minute) = (num(11, 2)?, num(14, 2)?);
    if hour > 23 || minute > 59 {
        return None;
    }
    ms += i64::from(hour) * 3_600_000 + i64::from(minute) * 60_000;
    let mut at = 16;
    if b.get(at) == Some(&b':') {
        let second = num(at + 1, 2)?;
        if second > 59 {
            return None;
        }
        ms += i64::from(second) * 1000;
        at += 3;
        if b.get(at) == Some(&b'.') {
            let start = at + 1;
            let mut end = start;
            while b.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == start {
                return None;
            }
            let padded = format!("{:0<3}", &text[start..end]);
            ms += padded[..3].parse::<i64>().ok()?;
            at = end;
        }
    }
    match &text[at..] {
        "Z" => Some((ms, Shape::DateTime)),
        zone if zone.len() == 6 && matches!(b[at], b'+' | b'-') && b[at + 3] == b':' => {
            let (oh, om) = (num(at + 1, 2)?, num(at + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let offset = (i64::from(oh) * 60 + i64::from(om)) * 60_000;
            Some((
                if b[at] == b'-' {
                    ms + offset
                } else {
                    ms - offset
                },
                Shape::DateTime,
            ))
        }
        _ => None,
    }
}

/// time/parse - ISO text to a `date` (text with no time) or a `datetime` (text with a time and zone); unparseable text answers null
pub struct TimeParseOp;
impl Operator for TimeParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/parse", 1);
        match eval_expr(&args[0], ctx)? {
            Value::String(text) => match parse_iso(&text) {
                Some((ms, shape)) => render_moment(ms as f64, shape),
                None => Ok(Value::Null),
            },
            other => Err(OrbitalError::Eval(other.kind_error("string"))),
        }
    }
}

/// time/valid - true iff time/parse would succeed on a string; a non-string input is a type mismatch
pub struct TimeValidOp;
impl Operator for TimeValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/valid", 1);
        let input = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(parse_iso(input.text()?).is_some()))
    }
}

/// time/format - format a date or datetime with moment-style tokens, in UTC
pub struct TimeFormatOp;
impl Operator for TimeFormatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/format", >= 2);
        let m = Moment::of(&eval_expr(&args[0], ctx)?)?;
        let format = eval_expr(&args[1], ctx)?;
        Ok(Value::string(format_moment_tokens(m, format.text()?)?))
    }
}

pub(super) fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

macro_rules! calendar_part {
    ($name:ident, $op:literal, $at:ident, $body:expr) => {
        #[doc = concat!($op, " - read one calendar part of a date or datetime (UTC)")]
        pub struct $name;
        impl Operator for $name {
            fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
                require_arity!(args, $op, 1);
                let m = Moment::of(&eval_expr(&args[0], ctx)?)?;
                let $at = utc(m.ms)?;
                Ok(Value::Number($body as f64))
            }
        }
    };
}

macro_rules! time_of_day_part {
    ($name:ident, $op:literal, $at:ident, $body:expr) => {
        #[doc = concat!($op, " - read the time of day of a datetime (UTC); a date refuses")]
        pub struct $name;
        impl Operator for $name {
            fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
                require_arity!(args, $op, 1);
                let ms = Moment::of(&eval_expr(&args[0], ctx)?)?.at_time_of_day($op)?;
                let $at = utc(ms)?;
                Ok(Value::Number($body as f64))
            }
        }
    };
}

calendar_part!(TimeYearOp, "time/year", at, chrono::Datelike::year(&at));
calendar_part!(TimeMonthOp, "time/month", at, chrono::Datelike::month(&at));
calendar_part!(TimeDayOp, "time/day", at, chrono::Datelike::day(&at));
calendar_part!(
    TimeWeekdayOp,
    "time/weekday",
    at,
    chrono::Datelike::weekday(&at).num_days_from_sunday()
);
time_of_day_part!(TimeHourOp, "time/hour", at, chrono::Timelike::hour(&at));
time_of_day_part!(
    TimeMinuteOp,
    "time/minute",
    at,
    chrono::Timelike::minute(&at)
);
time_of_day_part!(
    TimeSecondOp,
    "time/second",
    at,
    chrono::Timelike::second(&at)
);

/// time/isBetween - start <= date <= end
pub struct TimeIsBetweenOp;
impl Operator for TimeIsBetweenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isBetween", >= 3);
        let ts = Moment::of(&eval_expr(&args[0], ctx)?)?;
        let start = Moment::of(&eval_expr(&args[1], ctx)?)?;
        let end = Moment::of(&eval_expr(&args[2], ctx)?)?;
        same_shape(ts, start, "time/isBetween")?;
        same_shape(ts, end, "time/isBetween")?;
        Ok(Value::Bool(ts.ms >= start.ms && ts.ms <= end.ms))
    }
}

/// time/isSame - the same moment, or the same unit-bucket with a third argument
pub struct TimeIsSameOp;
impl Operator for TimeIsSameOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isSame", >= 2);
        let (a, b) = pair(args, ctx, "time/isSame")?;
        if args.len() > 2 {
            let unit = unit_arg(&args[2], ctx)?;
            if unit.below_a_day() && (a.shape == Shape::Date || b.shape == Shape::Date) {
                return Err(needs_time_of_day("time/isSame"));
            }
            Ok(Value::Bool(start_of(a.ms, unit) == start_of(b.ms, unit)))
        } else {
            Ok(Value::Bool((a.ms - b.ms).abs() < 1.0))
        }
    }
}

pub(super) fn timestamp_to_date(ts: f64) -> (i32, i32, i32) {
    let days = (ts / 86_400_000.0).floor() as i64;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as i32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

/// Epoch ms of a calendar day at 00:00:00 UTC.
pub(super) fn date_to_timestamp(year: i32, month: i32, day: i32) -> f64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    ((era * 146_097 + doe - 719_468) as f64) * 86_400_000.0
}

/// Moment-style token replacement (longest token first). A time-of-day token on a date refuses.
pub(super) fn format_moment_tokens(m: Moment, format: &str) -> Result<String, OrbitalError> {
    let dt = utc(m.ms)?;
    use chrono::{Datelike, Timelike};

    let weekday_short = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let weekday_long = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let month_short = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month_long = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    // `.num_days_from_sunday()` matches JS `Date.getDay()` (0 = Sunday).
    let dow = dt.weekday().num_days_from_sunday() as usize;
    let month_idx = (dt.month() - 1) as usize;

    // Longest-first so a scan at any position never matches a short token
    // (`M`/`D`) inside a longer one (`MMMM`/`DD`) — same rule the JS side
    // enforces by sorting its regex alternation the same way.
    let tokens: &[(&str, String)] = &[
        ("YYYY", format!("{:04}", dt.year())),
        ("MMMM", month_long[month_idx].to_string()),
        ("dddd", weekday_long[dow].to_string()),
        ("MMM", month_short[month_idx].to_string()),
        ("ddd", weekday_short[dow].to_string()),
        ("YY", format!("{:02}", dt.year().rem_euclid(100))),
        ("MM", format!("{:02}", dt.month())),
        ("DD", format!("{:02}", dt.day())),
        ("HH", format!("{:02}", dt.hour())),
        ("mm", format!("{:02}", dt.minute())),
        ("ss", format!("{:02}", dt.second())),
        ("M", dt.month().to_string()),
        ("D", dt.day().to_string()),
        ("H", dt.hour().to_string()),
        ("m", dt.minute().to_string()),
        ("s", dt.second().to_string()),
    ];

    let bytes = format.as_bytes();
    let mut out = String::with_capacity(format.len());
    let mut i = 0;
    'outer: while i < bytes.len() {
        for (token, value) in tokens {
            if format[i..].starts_with(token) {
                if m.shape == Shape::Date && matches!(*token, "HH" | "mm" | "ss" | "H" | "m" | "s")
                {
                    return Err(needs_time_of_day("time/format"));
                }
                out.push_str(value);
                i += token.len();
                continue 'outer;
            }
        }
        // `format` is ASCII token syntax; char_indices avoids splitting a
        // multi-byte char if the literal text around the tokens isn't.
        let ch = format[i..].chars().next().expect("i < bytes.len()");
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(out)
}

/// time/isPast - an instant before now; a date carries no instant and refuses
pub struct TimeIsPastOp;
impl Operator for TimeIsPastOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isPast", 1);
        let ts = Moment::of(&eval_expr(&args[0], ctx)?)?.at_time_of_day("time/isPast")?;
        Ok(Value::Bool(ts < ctx.now_millis()))
    }
}

/// time/isFuture - an instant after now; a date carries no instant and refuses
pub struct TimeIsFutureOp;
impl Operator for TimeIsFutureOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isFuture", 1);
        let ts = Moment::of(&eval_expr(&args[0], ctx)?)?.at_time_of_day("time/isFuture")?;
        Ok(Value::Bool(ts > ctx.now_millis()))
    }
}

/// time/isToday - the date or datetime falls on today's UTC date
pub struct TimeIsTodayOp;
impl Operator for TimeIsTodayOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isToday", 1);
        let ts = Moment::of(&eval_expr(&args[0], ctx)?)?.ms;
        const MS_PER_DAY: f64 = 86400000.0;
        Ok(Value::Bool(
            (ts / MS_PER_DAY).floor() == (ctx.now_millis() / MS_PER_DAY).floor(),
        ))
    }
}

/// time/duration - Format duration human-readable
pub struct TimeDurationOp;
impl Operator for TimeDurationOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/duration", 1);
        let ms = require_number!(eval_expr(&args[0], ctx)?);

        let total_seconds = (ms / 1000.0).abs() as u64;
        let days = total_seconds / 86400;
        let hours = (total_seconds % 86400) / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;

        let mut parts = Vec::new();
        if days > 0 {
            parts.push(format!("{}d", days));
        }
        if hours > 0 {
            parts.push(format!("{}h", hours));
        }
        if minutes > 0 {
            parts.push(format!("{}m", minutes));
        }
        if seconds > 0 || parts.is_empty() {
            parts.push(format!("{}s", seconds));
        }

        Ok(Value::string(parts.join(" ")))
    }
}
