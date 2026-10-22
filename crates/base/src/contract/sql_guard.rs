//! Read-only SQL enforcement, the one guard: the `postgres` effect's validator checks a literal statement with it and the
//! kernel checks every statement before it connects. It is the twin of `assertReadOnlySelect` (`sql-guard.ts`). It
//! accepts exactly one SELECT statement (optionally CTE-led). Comments are stripped and string and identifier
//! contents blanked before the keyword check, so denied words inside literals never false-positive and comment
//! or quote tricks cannot smuggle a second statement past it.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardResult {
    pub ok: bool,
    pub reason: Option<String>,
}

const DENIED_KEYWORDS: [&str; 6] = ["INSERT", "UPDATE", "DELETE", "MERGE", "INTO", "FOR"];

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// JS `String.prototype.trim`: Unicode white space and line terminators, and the BOM, but not U+0085.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

fn skip_single_quoted(sql: &[char], start: usize, e_string: bool) -> usize {
    let mut i = start + 1;
    while i < sql.len() {
        if e_string && sql[i] == '\\' {
            i += 2;
            continue;
        }
        if sql[i] == '\'' {
            if sql.get(i + 1) == Some(&'\'') {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    sql.len()
}

fn skip_double_quoted(sql: &[char], start: usize) -> usize {
    let mut i = start + 1;
    while i < sql.len() {
        if sql[i] == '"' {
            if sql.get(i + 1) == Some(&'"') {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    sql.len()
}

/// A dollar-quote tag (`$$` or `$tag$`) at `start`; `$1` placeholders are not tags.
fn match_dollar_tag(sql: &[char], start: usize) -> Option<Vec<char>> {
    let mut i = start + 1;
    if sql.get(i) == Some(&'$') {
        return Some(vec!['$', '$']);
    }
    let first = *sql.get(i)?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    i += 1;
    while i < sql.len() && is_word_char(sql[i]) && sql[i] != '$' {
        i += 1;
    }
    if sql.get(i) != Some(&'$') {
        return None;
    }
    Some(sql[start..=i].to_vec())
}

fn find_tag(sql: &[char], tag: &[char], from: usize) -> Option<usize> {
    (from..sql.len().saturating_sub(tag.len().saturating_sub(1)))
        .find(|&at| sql[at..].starts_with(tag))
}

fn sanitize(source: &str) -> String {
    let sql: Vec<char> = source.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < sql.len() {
        let ch = sql[i];
        if ch == '-' && sql.get(i + 1) == Some(&'-') {
            while i < sql.len() && sql[i] != '\n' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        if ch == '/' && sql.get(i + 1) == Some(&'*') {
            let mut depth = 1;
            i += 2;
            while i < sql.len() && depth > 0 {
                if sql[i] == '/' && sql.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if sql[i] == '*' && sql.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push(' ');
            continue;
        }
        if ch == '\'' {
            let mut j = i as isize - 1;
            while j >= 0 && matches!(sql[j as usize], ' ' | '\t' | '\n' | '\r') {
                j -= 1;
            }
            let e_string = j >= 0
                && matches!(sql[j as usize], 'e' | 'E')
                && (j == 0 || !is_word_char(sql[j as usize - 1]));
            i = skip_single_quoted(&sql, i, e_string);
            out.push_str("''");
            continue;
        }
        if ch == '"' {
            i = skip_double_quoted(&sql, i);
            out.push_str("\"\"");
            continue;
        }
        if ch == '$' {
            if let Some(tag) = match_dollar_tag(&sql, i) {
                i = match find_tag(&sql, &tag, i + tag.len()) {
                    Some(close) => close + tag.len(),
                    None => sql.len(),
                };
                out.push_str("$$");
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// The ASCII identifier-like words of `statement`, upper-cased: maximal runs of `[A-Za-z0-9_]` that do not begin
/// with a digit (the `\b[A-Za-z_][A-Za-z0-9_]*\b` of the TS guard, ASCII word boundaries).
fn sql_words(statement: &str) -> Vec<String> {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut words = Vec::new();
    let mut run = String::new();
    for c in statement.chars().chain(std::iter::once(' ')) {
        if is_word(c) {
            run.push(c);
        } else if !run.is_empty() {
            if !run.starts_with(|f: char| f.is_ascii_digit()) {
                words.push(run.to_ascii_uppercase());
            }
            run.clear();
        }
    }
    words
}

/// Verify `sql` is a single read-only SELECT statement.
pub fn assert_read_only_select(sql: &str) -> GuardResult {
    let fail = |reason: String| GuardResult {
        ok: false,
        reason: Some(reason),
    };
    let sanitized = sanitize(sql);
    let statements: Vec<&str> = sanitized
        .split(';')
        .map(js_trim)
        .filter(|s| !s.is_empty())
        .collect();
    match statements.len() {
        0 => return fail("Empty statement".into()),
        1 => {}
        _ => return fail("Only a single statement is allowed".into()),
    }
    let words = sql_words(statements[0]);
    match words.first().map(String::as_str) {
        Some("SELECT") | Some("WITH") => {}
        _ => return fail("Only SELECT statements are allowed".into()),
    }
    if let Some(denied) = words.iter().find(|w| DENIED_KEYWORDS.contains(&w.as_str())) {
        return fail(format!(
            "Keyword not allowed in a read-only query: {denied}"
        ));
    }
    GuardResult {
        ok: true,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_select_and_rejects_a_second_statement() {
        assert!(assert_read_only_select("SELECT 1;").ok);
        assert_eq!(
            assert_read_only_select("SELECT 1; SELECT 2")
                .reason
                .as_deref(),
            Some("Only a single statement is allowed")
        );
    }

    #[test]
    fn a_denied_word_inside_a_literal_is_inert_but_outside_it_is_not() {
        assert!(assert_read_only_select("SELECT 'delete into'").ok);
        assert!(!assert_read_only_select("SELECT x FROM t FOR UPDATE").ok);
    }

    #[test]
    fn the_byte_order_mark_trims_but_the_next_line_character_does_not() {
        assert_eq!(
            assert_read_only_select("\u{feff}").reason.as_deref(),
            Some("Empty statement")
        );
        assert_eq!(
            assert_read_only_select("\u{85}").reason.as_deref(),
            Some("Only SELECT statements are allowed")
        );
    }

    #[test]
    fn word_boundaries_are_ascii_like_javascript() {
        assert!(assert_read_only_select("\u{e9}SELECT 1").ok);
    }

    #[test]
    fn comments_and_quoted_text_never_hide_a_second_statement_or_a_denied_word() {
        for ok in [
            "SELECT 1 -- DROP TABLE users\n",
            "/* DELETE FROM users */ SELECT 1",
            "/* nested /* comment */ still comment DELETE */ SELECT 1",
            "WITH recent AS (SELECT * FROM events) SELECT * FROM recent",
            "SELECT $$ ; DELETE $$, $tag$ INSERT $tag$",
            "SELECT \"delete\" FROM t",
            "SELECT E'it\\'s; DELETE' AS x",
            "SELECT * FROM orders WHERE id = $1 AND total > $2;",
        ] {
            assert!(assert_read_only_select(ok).ok, "{ok}");
        }
        for (bad, why) in [
            ("SELECT 1; DELETE FROM t", "Only a single statement is allowed"),
            ("DELETE FROM t", "Only SELECT statements are allowed"),
            ("SELECT * INTO copy FROM t", "Keyword not allowed in a read-only query: INTO"),
            ("WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x", "Keyword not allowed in a read-only query: DELETE"),
            ("", "Empty statement"),
            ("  ;  ", "Empty statement"),
        ] {
            assert_eq!(assert_read_only_select(bad).reason.as_deref(), Some(why), "{bad}");
        }
    }

    #[test]
    fn a_word_that_starts_with_a_digit_is_not_a_keyword() {
        assert!(assert_read_only_select("SELECT 1update, x2insert FROM t").ok);
        assert!(!assert_read_only_select("SELECT 1 FROM t FOR SHARE").ok);
    }
}
