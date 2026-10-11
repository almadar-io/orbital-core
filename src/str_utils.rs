//! Canonical case-conversion helpers shared across the Orbital crates.
//!
//! Each function here is the single source of truth for a string-shape
//! transformation that was previously duplicated byte-for-byte across
//! orbital-cli, orbital-compiler, and orbital-shell-typescript.

/// PascalCase / camelCase → snake_case. Lowercases each uppercase char and
/// inserts `_` before it (except at the start). Separators in the input are
/// left untouched.
pub fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// PascalCase / camelCase → kebab-case, acronym-aware. A hyphen is inserted
/// before an uppercase letter when the previous char is lowercase (word
/// boundary) or when it is the last char of an acronym immediately followed
/// by a lowercase letter.
pub fn to_kebab_case(s: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            // Check if this is part of an acronym (consecutive uppercase letters)
            let prev_is_upper = i > 0 && chars[i - 1].is_uppercase();
            let next_is_upper = i + 1 < chars.len() && chars[i + 1].is_uppercase();
            let next_is_end = i + 1 >= chars.len();

            // Add hyphen before uppercase if:
            // - Not at start
            // - Previous char is lowercase (start of new word), OR
            // - This is the last char of an acronym followed by lowercase
            if i > 0 {
                let should_add_hyphen = !prev_is_upper || (!next_is_upper && !next_is_end);
                if should_add_hyphen {
                    result.push('-');
                }
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Lowercase the first character; leave the rest unchanged.
pub fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        result.push(first.to_ascii_lowercase());
    }
    result.extend(chars);
    result
}

/// Separator-aware PascalCase. Uppercases the char after any `_`, `-`, or ` `
/// (and the first char) and lowercases every other char.
///
/// Note: this is **not** the only PascalCase variant in the tree. The
/// `_`-split, case-preserving converter in orbital-compiler's test_generator
/// and the `-`-split, case-preserving converter in orbital-shell-typescript's
/// page_generator are behaviorally distinct (see this module's tests) and are
/// intentionally kept local to those callers rather than merged here.
pub fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c.to_ascii_lowercase());
        }
    }
    result
}

/// PascalCase constrained to a valid target-language identifier: every
/// non-alphanumeric char (not just `_`/`-`/` `) is a separator and is dropped.
/// Use where the result names a FUNCTION/SYMBOL in generated code — event keys
/// are not guaranteed identifier-safe (an unbound config-forward outlet keeps
/// its literal `@config.<knob>` key all the way into codegen, and
/// `to_pascal_case` would pass `@` and `.` through into the symbol name).
pub fn to_identifier_pascal(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;
    for c in s.chars() {
        if !c.is_ascii_alphanumeric() {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c.to_ascii_lowercase());
        }
    }
    result
}

/// PascalCase / camelCase → kebab-case, NAIVE (not acronym-aware): inserts `-`
/// before every uppercase char (except the first). For simple identifiers
/// (`TaskItem` → `task-item`) this matches `to_kebab_case`, but it differs on
/// acronyms (`HTMLForm` → `h-t-m-l-form`, not `html-form`). Used where callers
/// historically relied on the naive form (python URL paths, cli trait paths);
/// kept distinct from the acronym-aware `to_kebab_case` so neither drifts.
pub fn to_kebab_case_simple(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                result.push('-');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Uppercase the first character (ASCII); leave the rest unchanged
/// (`"unitType"` → `"UnitType"`, `"type"` → `"Type"`). The inverse of
/// `to_camel_case` for the first char only — does NOT lowercase the tail (so
/// it is distinct from the separator-aware `to_pascal_case`).
pub fn capitalize_first(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        result.push(first.to_ascii_uppercase());
    }
    result.extend(chars);
    result
}

/// kebab-case → PascalCase. Uppercases the first char of each `-`-split part
/// and keeps the remaining chars unchanged (e.g. "signature-capture" →
/// "SignatureCapture", "trash-2" → "Trash2").
pub fn kebab_to_pascal(s: &str) -> String {
    s.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
            }
        })
        .collect()
}

/// Separator-split PascalCase that **preserves the tail** of each word.
/// Splits on `_`, `-`, and ` `; uppercases the first char of every part and
/// leaves the remaining chars unchanged.
///
/// Unlike `to_pascal_case` (which lowercases the tail so `"fooBar"` → `"Foobar"`),
/// this is the right choice for code generators that receive OIR names which are
/// already well-capitalised: `"TaskItem"` → `"TaskItem"` (identity), `"task_item"`
/// → `"TaskItem"`, `"http_server"` → `"HttpServer"`.
///
/// This is the form used byte-identically in `orbital-shell-android` and
/// `orbital-shell-ios`; centralised here so both can route to one owner.
pub fn split_to_pascal_case(s: &str) -> String {
    s.split(['_', '-', ' '])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

/// Separator-split camelCase that **preserves the tail** of each word.
/// The first word is emitted unchanged; subsequent words have their first char
/// uppercased (e.g. `"task_item"` → `"taskItem"`, `"myField"` → `"myField"`).
///
/// Like `split_to_pascal_case`, this preserves the inner casing of each word —
/// it does NOT lowercase the whole input first. Centralised from the
/// byte-identical copies in `orbital-shell-android` and `orbital-shell-ios`.
pub fn split_to_camel_case(s: &str) -> String {
    let mut out = String::new();
    let mut capitalize_next = false;
    for c in s.chars() {
        if c == '_' || c == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            out.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits on `-`, `_`, and ` `, uppercases the first char of every word, and
/// joins with a single space.  `"my-app"` → `"My App"`, `"task_list"` →
/// `"Task List"`.
pub fn to_title_case(s: &str) -> String {
    s.split(['-', '_', ' '])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Escapes a string for embedding inside a double-quoted literal in any
/// language whose string syntax follows the C escape conventions (Kotlin,
/// Swift, TypeScript, JSON, …).
///
/// Escapes: `\` → `\\`, `"` → `\"`, newline → `\n`, carriage-return → `\r`,
/// tab → `\t`.  Language-specific escapes (e.g. Kotlin `$`) are NOT added here;
/// callers that need them should apply an additional pass on top.
pub fn escape_double_quoted(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Escapes a string for embedding inside a JavaScript/TypeScript string literal
/// (either double- or single-quoted).  Escapes `\`, `"`, `'`, newline,
/// carriage-return, and tab.  Use `escape_double_quoted` when the surrounding
/// language does not use single-quote string syntax.
pub fn escape_js_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Escapes a string for embedding inside a Python double-quoted string literal.
/// Escapes: `\` → `\\`, `"` → `\"`, newline → `\n`, tab → `\t`,
/// carriage-return → `\r`, and any other ASCII control char (U+0000–U+001F,
/// U+007F) as `\xNN` (two uppercase hex digits).
pub fn escape_python_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                out.push_str(&format!("\\x{:02X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Sanitises `name` into a valid identifier for a language whose identifiers
/// follow the pattern `[A-Za-z0-9_]+` with no leading digit.
///
/// Steps:
/// 1. Replace `-` with `_`.
/// 2. Keep only alphanumeric characters and `_`.
/// 3. Fall back to `"_empty"` if the result is empty (a bare `_` is reserved
///    in Swift and Kotlin).
/// 4. Suffix with `_` if the sanitised name appears in `keywords`.
///
/// The `keywords` slice is owned by the caller so that each shell backend can
/// supply its own reserved-word list (Kotlin, Swift, TypeScript, …) without
/// baking language-specific data into this shared helper.
pub fn sanitize_identifier(name: &str, keywords: &[&str]) -> String {
    let s = name.replace('-', "_");
    let sanitized: String = s
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if sanitized.is_empty() {
        return "_empty".to_string();
    }
    if keywords.contains(&sanitized.as_str()) {
        format!("{}_", sanitized)
    } else {
        sanitized
    }
}

/// A number as JavaScript's `String(n)` prints it (ECMA-262 Number::toString):
/// shortest round-trip digits, exponent form below 1e-6 and from 1e21.
pub fn js_number_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    let sign = if n < 0.0 { "-" } else { "" };
    let sci = format!("{:e}", n.abs());
    let (mantissa, exp) = sci
        .split_once('e')
        .expect("LowerExp always has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let point = exp.parse::<i32>().expect("LowerExp exponent is an integer") + 1;
    let body = if k <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        format!(
            "{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let e = point - 1;
        let e = if e >= 0 {
            format!("+{e}")
        } else {
            e.to_string()
        };
        if k == 1 {
            format!("{digits}e{e}")
        } else {
            format!("{}.{}e{e}", &digits[..1], &digits[1..])
        }
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_number_string_matches_ecma_number_to_string() {
        let cases = [
            (0.0, "0"),
            (-0.0, "0"),
            (3.0, "3"),
            (-2.0, "-2"),
            (3.5, "3.5"),
            (0.25, "0.25"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e21, "1e+21"),
            (1.25e22, "1.25e+22"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (n, want) in cases {
            assert_eq!(js_number_string(n), want, "{n:?}");
        }
    }

    #[test]
    fn control_js_number_string_is_not_rust_display() {
        assert_ne!(js_number_string(1e-7), format!("{}", 1e-7));
        assert_eq!(js_number_string(42.0), format!("{}", 42.0));
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("Task"), "task");
        assert_eq!(to_snake_case("TaskItem"), "task_item");
    }

    #[test]
    fn test_to_kebab_case() {
        assert_eq!(to_kebab_case("TaskItem"), "task-item");
        assert_eq!(to_kebab_case("HTMLParser"), "html-parser");
    }

    #[test]
    fn test_to_camel_case() {
        assert_eq!(to_camel_case("TaskItem"), "taskItem");
    }

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("task_item"), "TaskItem");
        assert_eq!(to_pascal_case("my-thing"), "MyThing");
    }

    #[test]
    fn test_kebab_to_pascal() {
        assert_eq!(kebab_to_pascal("chevron-right"), "ChevronRight");
        assert_eq!(kebab_to_pascal("plus"), "Plus");
        assert_eq!(kebab_to_pascal("trash-2"), "Trash2");
    }

    #[test]
    fn test_to_kebab_case_simple() {
        assert_eq!(to_kebab_case_simple("TaskItem"), "task-item");
        // diverges from the acronym-aware variant (the reason both exist)
        assert_eq!(to_kebab_case_simple("HTMLParser"), "h-t-m-l-parser");
        assert_eq!(to_kebab_case("HTMLParser"), "html-parser");
    }

    #[test]
    fn test_capitalize_first() {
        assert_eq!(capitalize_first("unitType"), "UnitType");
        assert_eq!(capitalize_first("type"), "Type");
        // does NOT lowercase the tail (unlike to_pascal_case)
        assert_eq!(capitalize_first("fooBar"), "FooBar");
        assert_eq!(capitalize_first(""), "");
    }

    /// The three PascalCase converters in the tree diverge; this pins the
    /// distinction so a future merge attempt fails loudly.
    #[test]
    fn pascal_case_variants_diverge() {
        // separator-aware (this module) lowercases the tail
        assert_eq!(to_pascal_case("fooBar"), "Foobar");
        // the compiler/page-generator variants preserve the tail ("FooBar"),
        // which is why they are not folded into to_pascal_case.
    }

    #[test]
    fn test_to_identifier_pascal() {
        // matches to_pascal_case on identifier-safe keys
        assert_eq!(to_identifier_pascal("MEMORIZED"), "Memorized");
        assert_eq!(to_identifier_pascal("item_click"), "ItemClick");
        // every non-alphanumeric char is a separator — a raw config sigil
        // still yields a valid generated-code symbol
        assert_eq!(
            to_identifier_pascal("@config.itemClickEvent"),
            "ConfigItemclickevent"
        );
        assert_eq!(
            to_identifier_pascal("Orbital.Trait.EVENT"),
            "OrbitalTraitEvent"
        );
    }

    #[test]
    fn test_split_to_pascal_case() {
        assert_eq!(split_to_pascal_case("task_item"), "TaskItem");
        assert_eq!(split_to_pascal_case("my-thing"), "MyThing");
        assert_eq!(split_to_pascal_case("my thing"), "MyThing");
        // already PascalCase — identity (tail preserved)
        assert_eq!(split_to_pascal_case("TaskItem"), "TaskItem");
        assert_eq!(split_to_pascal_case("http_server"), "HttpServer");
        // diverges from to_pascal_case: tail is NOT lowercased
        assert_eq!(split_to_pascal_case("fooBAR"), "FooBAR");
        assert_ne!(to_pascal_case("fooBAR"), split_to_pascal_case("fooBAR"));
    }

    #[test]
    fn test_split_to_camel_case() {
        assert_eq!(split_to_camel_case("task_item"), "taskItem");
        assert_eq!(split_to_camel_case("my-field"), "myField");
        // already camelCase — identity
        assert_eq!(split_to_camel_case("taskItem"), "taskItem");
        // single word unchanged
        assert_eq!(split_to_camel_case("task"), "task");
    }

    #[test]
    fn test_to_title_case() {
        assert_eq!(to_title_case("my-app"), "My App");
        assert_eq!(to_title_case("task_list"), "Task List");
        assert_eq!(to_title_case("hello world"), "Hello World");
        assert_eq!(to_title_case("single"), "Single");
    }

    #[test]
    fn test_escape_double_quoted() {
        assert_eq!(escape_double_quoted(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_double_quoted("line1\nline2"), r"line1\nline2");
        assert_eq!(escape_double_quoted("tab\there"), r"tab\there");
        assert_eq!(escape_double_quoted("back\\slash"), r"back\\slash");
        assert_eq!(escape_double_quoted("cr\rhere"), r"cr\rhere");
    }

    #[test]
    fn test_escape_python_string() {
        assert_eq!(escape_python_string(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_python_string("line1\nline2"), r"line1\nline2");
        assert_eq!(escape_python_string("tab\there"), r"tab\there");
        assert_eq!(escape_python_string("back\\slash"), r"back\\slash");
        assert_eq!(escape_python_string("cr\rhere"), r"cr\rhere");
        // control chars rendered as \xNN
        assert_eq!(escape_python_string("\x00\x01\x1F"), r"\x00\x01\x1F");
        assert_eq!(escape_python_string("\x7F"), r"\x7F");
        // regular chars pass through unchanged
        assert_eq!(escape_python_string("hello world"), "hello world");
        assert_eq!(escape_python_string("café"), "café");
    }

    #[test]
    fn test_sanitize_identifier() {
        let kotlin_kws = &["val", "var", "fun", "class", "object"];
        assert_eq!(sanitize_identifier("my-field", kotlin_kws), "my_field");
        assert_eq!(sanitize_identifier("val", kotlin_kws), "val_");
        assert_eq!(sanitize_identifier("myField", kotlin_kws), "myField");
        assert_eq!(sanitize_identifier("", kotlin_kws), "_empty");
        assert_eq!(sanitize_identifier("!!", kotlin_kws), "_empty");
        assert_eq!(sanitize_identifier("_", kotlin_kws), "_");
        assert_eq!(sanitize_identifier("123abc", kotlin_kws), "123abc");
        // keyword from a different language is not treated as reserved
        let swift_kws = &["let", "var", "func", "class", "struct"];
        assert_eq!(sanitize_identifier("let", swift_kws), "let_");
        assert_eq!(sanitize_identifier("let", kotlin_kws), "let");
    }
}
