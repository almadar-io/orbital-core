//! `format/*`: display formatting of numbers, percentages, currency, byte sizes, lists and phone numbers.
//!
//! Numbers format as `Intl.NumberFormat` does for the locales in [`locale_of`] and the currencies in [`CURRENCIES`]:
//! the shortest decimal digits of the value, rounded half away from zero at the last fraction digit, grouped by the
//! locale. A locale or currency outside those tables, and any option value outside its range, answers null, never a silent fallback.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

fn invalid(message: String) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(message))
}

/// A non-negative finite value as digits and a decimal point: the value is `0.DIGITS x 10^point`; zero has no digits.
struct Decimal {
    digits: Vec<u8>,
    point: i32,
}

impl Decimal {
    fn of(abs: f64) -> Self {
        if abs == 0.0 {
            return Self {
                digits: Vec::new(),
                point: 0,
            };
        }
        let text = format!("{abs:e}");
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let digits: Vec<u8> = mantissa
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
            .collect();
        Self {
            digits,
            point: exponent.parse::<i32>().unwrap_or(0) + 1,
        }
    }

    /// Round half away from zero at `fraction` digits after the point.
    fn rounded(mut self, fraction: i32) -> Self {
        let keep = self.point + fraction;
        if keep < 0 {
            return Self {
                digits: Vec::new(),
                point: 0,
            };
        }
        let keep = keep as usize;
        if keep >= self.digits.len() {
            return self;
        }
        let up = self.digits[keep] >= 5;
        self.digits.truncate(keep);
        if !up {
            return if self.digits.is_empty() {
                Self {
                    digits: Vec::new(),
                    point: 0,
                }
            } else {
                self
            };
        }
        if keep == 0 {
            return Self {
                digits: vec![1],
                point: 1 - fraction,
            };
        }
        let mut at = keep;
        while at > 0 {
            at -= 1;
            if self.digits[at] == 9 {
                self.digits[at] = 0;
            } else {
                self.digits[at] += 1;
                return self;
            }
        }
        Self {
            digits: vec![1],
            point: self.point + 1,
        }
    }

    fn shifted(mut self, places: i32) -> Self {
        if !self.digits.is_empty() {
            self.point += places;
        }
        self
    }

    fn integer(&self) -> String {
        if self.point <= 0 || self.digits.is_empty() {
            return "0".to_string();
        }
        let point = self.point as usize;
        (0..point)
            .map(|i| char::from(b'0' + self.digits.get(i).copied().unwrap_or(0)))
            .collect()
    }

    fn fraction(&self, min: usize, max: usize) -> String {
        let mut out: Vec<u8> = Vec::new();
        if self.point < 0 {
            out.extend(std::iter::repeat(0).take((-self.point) as usize));
        }
        let from = self.point.max(0) as usize;
        out.extend(self.digits.iter().skip(from));
        out.truncate(max);
        while out.len() > min && out.last() == Some(&0) {
            out.pop();
        }
        while out.len() < min {
            out.push(0);
        }
        out.into_iter().map(|d| char::from(b'0' + d)).collect()
    }
}

pub struct Locale {
    group: &'static str,
    decimal: &'static str,
    min_group: usize,
    minus: &'static str,
}

const EN: Locale = Locale {
    group: ",",
    decimal: ".",
    min_group: 1,
    minus: "-",
};
const DE: Locale = Locale {
    group: ".",
    decimal: ",",
    min_group: 1,
    minus: "-",
};
const FR: Locale = Locale {
    group: "\u{202f}",
    decimal: ",",
    min_group: 1,
    minus: "-",
};
const ES: Locale = Locale {
    group: ".",
    decimal: ",",
    min_group: 2,
    minus: "-",
};
const SL: Locale = Locale {
    group: ".",
    decimal: ",",
    min_group: 2,
    minus: "\u{2212}",
};
const AR: Locale = Locale {
    group: ",",
    decimal: ".",
    min_group: 1,
    minus: "\u{200e}-",
};
const RU: Locale = Locale {
    group: "\u{a0}",
    decimal: ",",
    min_group: 1,
    minus: "-",
};

/// The locales `format/number` knows: the number conventions of these tags, as `Intl.NumberFormat` writes them.
const LOCALES: &[(&str, &Locale)] = &[
    ("en", &EN),
    ("en-US", &EN),
    ("en-GB", &EN),
    ("en-CA", &EN),
    ("en-AU", &EN),
    ("ja", &EN),
    ("ja-JP", &EN),
    ("de", &DE),
    ("de-DE", &DE),
    ("pt", &DE),
    ("pt-BR", &DE),
    ("nl", &DE),
    ("nl-NL", &DE),
    ("fr", &FR),
    ("fr-FR", &FR),
    ("es", &ES),
    ("es-ES", &ES),
    ("it", &ES),
    ("it-IT", &ES),
    ("sl", &SL),
    ("sl-SI", &SL),
    ("ar", &AR),
    ("ru", &RU),
    ("ru-RU", &RU),
];

pub fn locale_of(tag: &str) -> Option<&'static Locale> {
    LOCALES
        .iter()
        .find(|(known, _)| *known == tag)
        .map(|(_, locale)| *locale)
}

/// ISO code, the symbol written before the amount in English, and the minor units the currency has.
const CURRENCIES: &[(&str, &str, usize)] = &[
    ("USD", "$", 2),
    ("EUR", "\u{20ac}", 2),
    ("GBP", "\u{a3}", 2),
    ("JPY", "\u{a5}", 0),
    ("CNY", "CN\u{a5}", 2),
    ("INR", "\u{20b9}", 2),
    ("KRW", "\u{20a9}", 0),
    ("CAD", "CA$", 2),
    ("AUD", "A$", 2),
    ("CHF", "CHF\u{a0}", 2),
    ("MXN", "MX$", 2),
    ("BRL", "R$", 2),
    ("SEK", "SEK\u{a0}", 2),
    ("NOK", "NOK\u{a0}", 2),
    ("DKK", "DKK\u{a0}", 2),
    ("PLN", "PLN\u{a0}", 2),
    ("CZK", "CZK\u{a0}", 2),
    ("ZAR", "ZAR\u{a0}", 2),
    ("NZD", "NZ$", 2),
    ("SGD", "SGD\u{a0}", 2),
    ("HKD", "HK$", 2),
    ("AED", "AED\u{a0}", 2),
    ("SAR", "SAR\u{a0}", 2),
    ("ILS", "\u{20aa}", 2),
    ("TRY", "TRY\u{a0}", 2),
    ("RUB", "RUB\u{a0}", 2),
    ("KWD", "KWD\u{a0}", 3),
    ("BHD", "BHD\u{a0}", 3),
    ("CLP", "CLP\u{a0}", 0),
    ("VND", "\u{20ab}", 0),
];

fn group(integer: &str, locale: &Locale) -> String {
    if integer.len() < 3 + locale.min_group {
        return integer.to_string();
    }
    let first = integer.len() % 3;
    let mut parts: Vec<&str> = Vec::new();
    if first > 0 {
        parts.push(&integer[..first]);
    }
    parts.extend(
        (first..integer.len())
            .step_by(3)
            .map(|at| &integer[at..at + 3]),
    );
    parts.join(locale.group)
}

/// A number the way the locale writes it with `min`..=`max` fraction digits; `shift` scales by a power of ten first.
fn render(n: f64, locale: &Locale, min: usize, max: usize, shift: i32) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    let sign = if n.is_sign_negative() {
        locale.minus
    } else {
        ""
    };
    if n.is_infinite() {
        return format!("{sign}\u{221e}");
    }
    let decimal = Decimal::of(n.abs()).shifted(shift).rounded(max as i32);
    let fraction = decimal.fraction(min, max);
    let integer = group(&decimal.integer(), locale);
    if fraction.is_empty() {
        format!("{sign}{integer}")
    } else {
        format!("{sign}{integer}{}{fraction}", locale.decimal)
    }
}

fn number_arg(expr: &SExpression, ctx: &BindingContext) -> Result<f64, OrbitalError> {
    Ok(require_number!(eval_expr(expr, ctx)?))
}

enum Digits {
    Absent,
    Set(usize),
    OutOfRange,
}

fn fraction_digits(options: &ValueMap, key: &str) -> Result<Digits, OrbitalError> {
    match options.get(key) {
        None | Some(Value::Null) => Ok(Digits::Absent),
        Some(Value::Number(n)) if n.fract() == 0.0 && (0.0..=100.0).contains(n) => {
            Ok(Digits::Set(*n as usize))
        }
        Some(Value::Number(_)) => Ok(Digits::OutOfRange),
        Some(other) => Err(invalid(format!(
            "format/number: `{key}` must be a whole number from 0 to 100, got {}",
            other.to_string_value()
        ))),
    }
}

/// format/number - locale grouping and fraction digits (`decimals`, or `minimumFractionDigits` / `maximumFractionDigits`)
pub struct FormatNumberOp;
impl Operator for FormatNumberOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/number", 1);
        let n = number_arg(&args[0], ctx)?;
        let options = match args.get(1) {
            Some(expr) => match eval_expr(expr, ctx)? {
                Value::Object(map) => map,
                Value::Null => ValueMap::default().into(),
                other => {
                    return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: "object".to_string(),
                        actual: other.type_name().to_string(),
                    }))
                }
            },
            None => ValueMap::default().into(),
        };
        let locale = match options.get("locale") {
            None | Some(Value::Null) => &EN,
            Some(Value::String(tag)) => match locale_of(tag) {
                Some(locale) => locale,
                None => return Ok(Value::Null),
            },
            Some(other) => {
                return Err(invalid(format!(
                    "format/number: `locale` must be a string, got {}",
                    other.to_string_value()
                )))
            }
        };
        let decimals = fraction_digits(&options, "decimals")?;
        let minimum = fraction_digits(&options, "minimumFractionDigits")?;
        let maximum = fraction_digits(&options, "maximumFractionDigits")?;
        let (min, max) = match (decimals, minimum, maximum) {
            (Digits::OutOfRange, _, _)
            | (_, Digits::OutOfRange, _)
            | (_, _, Digits::OutOfRange) => return Ok(Value::Null),
            (Digits::Set(decimals), _, _) => (decimals, decimals),
            (Digits::Absent, Digits::Set(min), Digits::Set(max)) if max < min => {
                return Ok(Value::Null)
            }
            (Digits::Absent, Digits::Set(min), Digits::Set(max)) => (min, max),
            (Digits::Absent, Digits::Set(min), Digits::Absent) => (min, min.max(3)),
            (Digits::Absent, Digits::Absent, Digits::Set(max)) => (0, max),
            (Digits::Absent, Digits::Absent, Digits::Absent) => (0, 3),
        };
        Ok(Value::string(render(n, locale, min, max, 0)))
    }
}

/// format/percent - the value times one hundred with `decimals` fraction digits and a percent sign
pub struct FormatPercentOp;
impl Operator for FormatPercentOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/percent", 1);
        let n = number_arg(&args[0], ctx)?;
        let decimals = match args.get(1) {
            Some(expr) => {
                let d = number_arg(expr, ctx)?;
                if d.fract() != 0.0 || !(0.0..=100.0).contains(&d) {
                    return Ok(Value::Null);
                }
                d as usize
            }
            None => 0,
        };
        Ok(Value::string(format!(
            "{}%",
            render(n, &EN, decimals, decimals, 2)
        )))
    }
}

/// format/currency - an amount in an ISO currency, written as English writes it
pub struct FormatCurrencyOp;
impl Operator for FormatCurrencyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/currency", >= 2);
        let n = number_arg(&args[0], ctx)?;
        let code = eval_expr(&args[1], ctx)?.to_string_value();
        let locale = match args.get(2) {
            Some(expr) => eval_expr(expr, ctx)?.to_string_value(),
            None => "en-US".to_string(),
        };
        if !matches!(locale.as_str(), "en" | "en-US") {
            return Ok(Value::Null);
        }
        let Some((_, symbol, digits)) = CURRENCIES.iter().find(|(known, _, _)| *known == code)
        else {
            return Ok(Value::Null);
        };
        let amount = render(n, &EN, *digits, *digits, 0);
        Ok(Value::string(match amount.strip_prefix('-') {
            Some(rest) => format!("-{symbol}{rest}"),
            None => format!("{symbol}{amount}"),
        }))
    }
}

/// JavaScript `Number.prototype.toFixed` over the exact binary value, ties to the larger magnitude.
fn to_fixed(x: f64, digits: usize) -> String {
    let exact = format!("{x:.60}");
    let (whole, fraction) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut kept: Vec<u8> = whole
        .bytes()
        .chain(fraction.bytes().take(digits))
        .map(|b| b - b'0')
        .collect();
    let up = fraction.as_bytes().get(digits).is_some_and(|d| *d >= b'5');
    if up {
        let mut at = kept.len();
        loop {
            if at == 0 {
                kept.insert(0, 1);
                break;
            }
            at -= 1;
            if kept[at] == 9 {
                kept[at] = 0;
            } else {
                kept[at] += 1;
                break;
            }
        }
    }
    let split = kept.len() - digits;
    let text: String = kept.iter().map(|d| char::from(b'0' + d)).collect();
    if digits == 0 {
        text
    } else {
        format!("{}.{}", &text[..split], &text[split..])
    }
}

/// A JavaScript number as `String(n)` writes a finite one below 1e21.
fn js_number(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// format/bytes - a size in B, KB, MB, GB, TB or PB (1024 steps), one decimal below ten, none from ten
pub struct FormatBytesOp;
impl Operator for FormatBytesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/bytes", 1);
        let bytes = number_arg(&args[0], ctx)?;
        if !bytes.is_finite() || bytes < 0.0 {
            return Ok(Value::Null);
        }
        if bytes == 0.0 {
            return Ok(Value::string("0 B"));
        }
        const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
        let step = ((bytes.ln() / 1024f64.ln()).floor() as i64).clamp(0, 5) as usize;
        if step == 0 {
            return Ok(Value::string(format!("{} B", js_number(bytes))));
        }
        let value = bytes / 1024f64.powi(step as i32);
        let text = if value >= 10.0 {
            js_number((value + 0.5).floor())
        } else if value.fract() == 0.0 {
            js_number(value)
        } else {
            to_fixed(value, 1)
        };
        Ok(Value::string(format!("{text} {}", UNITS[step])))
    }
}

/// format/list - items joined as natural language with `and` (or the given word)
pub struct FormatListOp;
impl Operator for FormatListOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/list", 1);
        let value = eval_expr(&args[0], ctx)?;
        let style = match args.get(1) {
            Some(expr) => eval_expr(expr, ctx)?.to_string_value(),
            None => "and".to_string(),
        };
        let items: Vec<String> = match value {
            Value::Array(items) => items.iter().map(Value::to_string_value).collect(),
            Value::Null => Vec::new(),
            other => return Ok(Value::string(other.to_string_value())),
        };
        Ok(Value::string(match items.len() {
            0 => String::new(),
            1 => items[0].to_string(),
            2 => format!("{} {style} {}", items[0], items[1]),
            _ => format!(
                "{}, {style} {}",
                items[..items.len() - 1].join(", "),
                items[items.len() - 1]
            ),
        }))
    }
}

/// format/phone - a phone number's digits grouped for the `US` format (the default) or as `ddd-ddd-rest`
pub struct FormatPhoneOp;
impl Operator for FormatPhoneOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/phone", 1);
        let text = eval_expr(&args[0], ctx)?.to_string_value();
        let format = match args.get(1) {
            Some(expr) => eval_expr(expr, ctx)?.to_string_value(),
            None => "US".to_string(),
        };
        let digits: String = text.chars().filter(char::is_ascii_digit).collect();
        let formatted = if format == "US" && digits.len() == 10 {
            format!("({}) {}-{}", &digits[0..3], &digits[3..6], &digits[6..])
        } else if format == "US" && digits.len() == 11 && digits.starts_with('1') {
            format!("+1 ({}) {}-{}", &digits[1..4], &digits[4..7], &digits[7..])
        } else if digits.len() >= 10 {
            format!("{}-{}-{}", &digits[0..3], &digits[3..6], &digits[6..])
        } else {
            text
        };
        Ok(Value::string(formatted))
    }
}
