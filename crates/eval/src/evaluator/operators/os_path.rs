//! `os/path-*`: string math on POSIX paths, identical on every host and never touching the filesystem.
//! (`path/` is taken by the A* operators.)
//!
//! Semantics: the separator is `/`. `normalize` collapses repeated separators and `.`, resolves `..`, drops a
//! trailing separator, lets `..` run past the root of an absolute path into the root, and keeps leading `..`
//! of a relative one; the empty path is `.`. `base` and `dir` strip trailing separators first (Node's `posix`
//! `basename`/`dirname`); `ext` is the last `.` of the base to its end, none for a base that starts with its
//! only dot. `relative` needs both paths absolute on one root or both relative to one shared base.
//! A path that starts with a drive (`C:/` or `C:\`) is absolute on that drive: the drive is its root, rendered
//! `C:/` with the letter uppercased, and `\` separates its components too. Every other path keeps `\` literal.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

fn path_arg(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<String, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::String(s) => Ok(s.into_inner()),
        other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: expected a string path, got {}",
            other.type_name()
        )))),
    }
}

/// The path with `/` separators, and the length of its root: `/` (1), a drive `C:/` (3) or none (0).
fn rooted(path: &str) -> (String, usize) {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        let rest = path[3..].replace('\\', "/");
        return (
            format!("{}:/{rest}", char::from(bytes[0].to_ascii_uppercase())),
            3,
        );
    }
    (path.to_string(), usize::from(path.starts_with('/')))
}

pub fn is_absolute(path: &str) -> bool {
    rooted(path).1 > 0
}

fn components_of(text: &str, root: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in text[root..].split('/') {
        match part {
            "" | "." => {}
            ".." => match out.last().map(String::as_str) {
                Some("..") => out.push("..".to_string()),
                Some(_) => {
                    out.pop();
                }
                None if root > 0 => {}
                None => out.push("..".to_string()),
            },
            name => out.push(name.to_string()),
        }
    }
    out
}

pub fn normalize(path: &str) -> String {
    let (text, root) = rooted(path);
    let parts = components_of(&text, root);
    match (root > 0, parts.is_empty()) {
        (true, _) => format!("{}{}", &text[..root], parts.join("/")),
        (false, true) => ".".to_string(),
        (false, false) => parts.join("/"),
    }
}

pub fn join(parts: &[String]) -> String {
    let joined = parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("/");
    normalize(&joined)
}

fn trimmed(text: &str, root: usize) -> &str {
    let stripped = text.trim_end_matches('/');
    if stripped.len() < root {
        &text[..root]
    } else {
        stripped
    }
}

pub fn base(path: &str) -> String {
    let (text, root) = rooted(path);
    let path = trimmed(&text, root);
    path[root..]
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

pub fn dir(path: &str) -> String {
    let (text, root) = rooted(path);
    let path = trimmed(&text, root);
    match path[root..].rfind('/') {
        None if root > 0 => path[..root].to_string(),
        None => ".".to_string(),
        Some(i) => trimmed(&path[..root + i], root).to_string(),
    }
}

pub fn ext(path: &str) -> String {
    let name = base(path);
    match name.rfind('.') {
        Some(i) if i > 0 && name != ".." => name[i..].to_string(),
        _ => String::new(),
    }
}

pub fn relative(from: &str, to: &str) -> Result<String, String> {
    let (from, from_root) = rooted(from);
    let (to, to_root) = rooted(to);
    if (from_root > 0) != (to_root > 0) {
        return Err("both paths must be absolute or both relative".to_string());
    }
    if from[..from_root] != to[..to_root] {
        return Err("the paths are on different roots".to_string());
    }
    let from = components_of(&from, from_root);
    let to = components_of(&to, to_root);
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if from[shared..].iter().any(|c| c == "..") {
        return Err(
            "the start path climbs above the shared base, so the route is unknown".to_string(),
        );
    }
    let mut steps: Vec<&str> = vec![".."; from.len() - shared];
    steps.extend(to[shared..].iter().map(String::as_str));
    Ok(if steps.is_empty() {
        ".".to_string()
    } else {
        steps.join("/")
    })
}

fn invalid(op: &str, message: String) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(format!("{op}: {message}")))
}

/// os/path-join - join the parts with `/`, then normalize; empty parts are skipped
pub struct OsPathJoinOp;
impl Operator for OsPathJoinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-join", >= 1);
        let parts = args
            .iter()
            .map(|a| path_arg(a, ctx, "os/path-join"))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Value::string(join(&parts)))
    }
}

/// os/path-normalize
pub struct OsPathNormalizeOp;
impl Operator for OsPathNormalizeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-normalize", == 1);
        Ok(Value::string(normalize(&path_arg(
            &args[0],
            ctx,
            "os/path-normalize",
        )?)))
    }
}

/// os/path-base - the last component
pub struct OsPathBaseOp;
impl Operator for OsPathBaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-base", == 1);
        Ok(Value::string(base(&path_arg(
            &args[0],
            ctx,
            "os/path-base",
        )?)))
    }
}

/// os/path-dir - everything before the last component
pub struct OsPathDirOp;
impl Operator for OsPathDirOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-dir", == 1);
        Ok(Value::string(dir(&path_arg(&args[0], ctx, "os/path-dir")?)))
    }
}

/// os/path-ext - the last component's extension, with its dot
pub struct OsPathExtOp;
impl Operator for OsPathExtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-ext", == 1);
        Ok(Value::string(ext(&path_arg(&args[0], ctx, "os/path-ext")?)))
    }
}

/// os/path-relative - the route from the first path to the second
pub struct OsPathRelativeOp;
impl Operator for OsPathRelativeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-relative", == 2);
        let from = path_arg(&args[0], ctx, "os/path-relative")?;
        let to = path_arg(&args[1], ctx, "os/path-relative")?;
        relative(&from, &to)
            .map(Value::string)
            .map_err(|message| invalid("os/path-relative", message))
    }
}

/// os/path-is-absolute
pub struct OsPathIsAbsoluteOp;
impl Operator for OsPathIsAbsoluteOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-is-absolute", == 1);
        Ok(Value::Bool(is_absolute(&path_arg(
            &args[0],
            ctx,
            "os/path-is-absolute",
        )?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(form: serde_json::Value) -> EvalResult {
        let expr: SExpression = serde_json::from_value(form).expect("s-expression");
        eval_expr(&expr, &BindingContext::default())
    }

    fn text(form: serde_json::Value) -> String {
        match eval(form) {
            Ok(Value::String(s)) => s.into_inner(),
            other => panic!("expected a string, got {other:?}"),
        }
    }

    #[test]
    fn normalize_collapses_separators_dots_and_dotdots() {
        assert_eq!(normalize("a//b///c"), "a/b/c");
        assert_eq!(normalize("/a/./b/../c"), "/a/c");
        assert_eq!(normalize("a/b/../../c"), "c");
        assert_eq!(normalize("a/b/"), "a/b");
        assert_eq!(normalize("/a/b/"), "/a/b");
    }

    #[test]
    fn normalize_empty_and_root_cases() {
        assert_eq!(normalize(""), ".");
        assert_eq!(normalize("."), ".");
        assert_eq!(normalize("./"), ".");
        assert_eq!(normalize("a/.."), ".");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("///"), "/");
        assert_eq!(normalize("/."), "/");
    }

    #[test]
    fn dotdot_past_the_root_of_an_absolute_path_stays_at_the_root_but_a_relative_one_keeps_it() {
        assert_eq!(normalize("/.."), "/");
        assert_eq!(normalize("/../../a"), "/a");
        assert_eq!(normalize("/a/../../b"), "/b");
        assert_eq!(normalize(".."), "..");
        assert_eq!(normalize("../.."), "../..");
        assert_eq!(normalize("../a/../.."), "../..");
        assert_eq!(normalize("a/../.."), "..");
    }

    #[test]
    fn join_skips_empty_parts_and_normalizes() {
        assert_eq!(join(&["a".into(), "b".into()]), "a/b");
        assert_eq!(join(&["a/".into(), "/b".into()]), "a/b");
        assert_eq!(join(&["/a".into(), "b/".into()]), "/a/b");
        assert_eq!(join(&["a".into(), "".into(), "b".into()]), "a/b");
        assert_eq!(join(&["a".into(), "..".into(), "b".into()]), "b");
        assert_eq!(join(&["/a".into(), "../..".into()]), "/");
        assert_eq!(join(&["".into()]), ".");
        assert_eq!(join(&[]), ".");
    }

    #[test]
    fn join_does_not_let_a_later_absolute_part_reset_the_path() {
        assert_eq!(join(&["/a".into(), "/b".into()]), "/a/b");
        assert_eq!(join(&["a".into(), "/b".into()]), "a/b");
    }

    #[test]
    fn base_takes_the_last_component() {
        assert_eq!(base("/a/b.txt"), "b.txt");
        assert_eq!(base("a/b/"), "b");
        assert_eq!(base("a//b//"), "b");
        assert_eq!(base("name"), "name");
        assert_eq!(base(""), "");
        assert_eq!(base("/"), "");
        assert_eq!(base("//"), "");
    }

    #[test]
    fn dir_takes_everything_before_the_last_component() {
        assert_eq!(dir("/a/b/c"), "/a/b");
        assert_eq!(dir("/a/b/c/"), "/a/b");
        assert_eq!(dir("a/b"), "a");
        assert_eq!(dir("a//b"), "a");
        assert_eq!(dir("name"), ".");
        assert_eq!(dir("/name"), "/");
        assert_eq!(dir("/"), "/");
        assert_eq!(dir(""), ".");
        assert_eq!(dir("//"), "/");
    }

    #[test]
    fn ext_is_the_last_dot_of_the_base() {
        assert_eq!(ext("/a/b.txt"), ".txt");
        assert_eq!(ext("a.tar.gz"), ".gz");
        assert_eq!(ext("a."), ".");
        assert_eq!(ext("/a.d/b"), "");
        assert_eq!(ext("a/b.txt/"), ".txt");
    }

    #[test]
    fn ext_of_a_dotfile_or_a_dot_name_or_nothing_is_empty() {
        assert_eq!(ext(".bashrc"), "");
        assert_eq!(ext("a/.bashrc"), "");
        assert_eq!(ext(".."), "");
        assert_eq!(ext("."), "");
        assert_eq!(ext(""), "");
        assert_eq!(ext("/"), "");
        assert_eq!(ext(".a.b"), ".b");
    }

    #[test]
    fn relative_climbs_to_the_shared_base() {
        assert_eq!(relative("/a/b", "/a/c/d"), Ok("../c/d".to_string()));
        assert_eq!(relative("/a/b", "/a/b/c"), Ok("c".to_string()));
        assert_eq!(relative("/a/b/c", "/a"), Ok("../..".to_string()));
        assert_eq!(relative("/a//b/", "/a/./b"), Ok(".".to_string()));
        assert_eq!(relative("a/b", "a/c"), Ok("../c".to_string()));
        assert_eq!(relative("/", "/a"), Ok("a".to_string()));
        assert_eq!(relative("/a", "/"), Ok("..".to_string()));
    }

    #[test]
    fn relative_refuses_what_needs_a_working_directory() {
        assert!(relative("/a", "b").is_err());
        assert!(relative("a", "/b").is_err());
        assert!(relative("../a", "b").is_err());
        assert_eq!(relative("../a", "../a/b"), Ok("b".to_string()));
        assert_eq!(relative("", "a"), Ok("a".to_string()));
    }

    #[test]
    fn absolute_means_a_leading_separator() {
        assert!(is_absolute("/"));
        assert!(is_absolute("/a"));
        assert!(is_absolute("//a"));
        assert!(!is_absolute("a/b"));
        assert!(!is_absolute(""));
        assert!(!is_absolute("./a"));
    }

    #[test]
    fn a_drive_rooted_path_is_absolute_on_its_drive_and_renders_with_slashes() {
        assert!(is_absolute(r"C:\a\b"));
        assert!(is_absolute("c:/a"));
        assert!(is_absolute(r"D:\"));
        assert!(!is_absolute("C:"));
        assert!(!is_absolute("C:a"));
        assert!(!is_absolute(r"\a"));
        assert!(!is_absolute(r"1:\a"));
        assert_eq!(normalize(r"c:\a\.\b\..\c\"), "C:/a/c");
        assert_eq!(normalize(r"C:\"), "C:/");
        assert_eq!(normalize("C:/a/../.."), "C:/");
        assert_eq!(normalize(r"C:\..\..\x"), "C:/x");
        assert_eq!(
            join(&[r"C:\a\2".into(), "ui/x.orb".into()]),
            "C:/a/2/ui/x.orb"
        );
        assert_eq!(
            join(&[r"C:\a\2".into(), r"ui\x.orb".into()]),
            "C:/a/2/ui/x.orb"
        );
    }

    #[test]
    fn a_backslash_outside_a_drive_rooted_path_is_an_ordinary_character() {
        assert_eq!(join(&[r"a\b".into(), "c".into()]), r"a\b/c");
        assert_eq!(normalize(r"/a\..\b"), r"/a\..\b");
        assert_eq!(base(r"/x/a\b"), r"a\b");
        assert_eq!(normalize("/a/C:/b"), "/a/C:/b");
        assert_eq!(join(&["/srv".into(), r"C:\x".into()]), r"/srv/C:\x");
    }

    #[test]
    fn base_dir_and_ext_of_a_drive_rooted_path() {
        assert_eq!(base(r"C:\a\b.txt"), "b.txt");
        assert_eq!(base(r"C:\"), "");
        assert_eq!(dir(r"C:\a\b.txt"), "C:/a");
        assert_eq!(dir(r"C:\a"), "C:/");
        assert_eq!(dir(r"C:\"), "C:/");
        assert_eq!(dir(r"C:\a\\b\"), "C:/a");
        assert_eq!(ext(r"C:\a.d\b.tar.gz"), ".gz");
    }

    #[test]
    fn relative_needs_one_root() {
        assert_eq!(relative(r"C:\a\b", r"c:\a\c\d"), Ok("../c/d".to_string()));
        assert_eq!(relative(r"C:\", r"C:\a"), Ok("a".to_string()));
        assert!(relative(r"C:\a", r"D:\a").is_err());
        assert!(relative(r"C:\a", "/a").is_err());
        assert!(relative(r"C:\a", "a").is_err());
    }

    #[test]
    fn the_operators_are_wired_by_name() {
        assert_eq!(
            text(serde_json::json!(["os/path-join", "/a", "b", "../c"])),
            "/a/c"
        );
        assert_eq!(
            text(serde_json::json!(["os/path-normalize", "a//b/../c/"])),
            "a/c"
        );
        assert_eq!(
            text(serde_json::json!(["os/path-base", "/a/b.txt"])),
            "b.txt"
        );
        assert_eq!(text(serde_json::json!(["os/path-dir", "/a/b.txt"])), "/a");
        assert_eq!(text(serde_json::json!(["os/path-ext", "/a/b.txt"])), ".txt");
        assert_eq!(
            text(serde_json::json!(["os/path-relative", "/a/b", "/a/c"])),
            "../c"
        );
        assert_eq!(
            eval(serde_json::json!(["os/path-is-absolute", "/a"])).ok(),
            Some(Value::Bool(true))
        );
        assert_eq!(
            eval(serde_json::json!(["os/path-is-absolute", "a"])).ok(),
            Some(Value::Bool(false))
        );
        assert_eq!(
            eval(serde_json::json!(["os/path-is-absolute", "C:\\a"])).ok(),
            Some(Value::Bool(true))
        );
    }

    #[test]
    fn a_wrong_arity_or_a_non_string_path_is_an_evaluation_error() {
        assert!(eval(serde_json::json!(["os/path-join"])).is_err());
        assert!(eval(serde_json::json!(["os/path-normalize"])).is_err());
        assert!(eval(serde_json::json!(["os/path-normalize", "a", "b"])).is_err());
        assert!(eval(serde_json::json!(["os/path-relative", "/a"])).is_err());
        assert!(eval(serde_json::json!(["os/path-base", 3])).is_err());
        assert!(eval(serde_json::json!(["os/path-join", "a", 1])).is_err());
        assert!(eval(serde_json::json!(["os/path-relative", "/a", "b"])).is_err());
    }
}
