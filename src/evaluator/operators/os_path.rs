//! `os/path-*`: string math on POSIX paths, identical on every host and never touching the filesystem.
//! (`path/` is taken by the A* operators.)
//!
//! Semantics: the separator is `/`. `normalize` collapses repeated separators and `.`, resolves `..`, drops a
//! trailing separator, lets `..` run past the root of an absolute path into the root, and keeps leading `..`
//! of a relative one; the empty path is `.`. `base` and `dir` strip trailing separators first (Node's `posix`
//! `basename`/`dirname`); `ext` is the last `.` of the base to its end, none for a base that starts with its
//! only dot. `relative` needs both paths absolute or both relative to one shared base.

use super::macros::*;
use super::*;

fn path_arg(arg: &SExpression, ctx: &BindingContext, op: &str) -> Result<String, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::String(s) => Ok(s),
        other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: expected a string path, got {}",
            other.type_name()
        )))),
    }
}

pub(crate) fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

fn components(path: &str) -> Vec<String> {
    let absolute = is_absolute(path);
    let mut out: Vec<String> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => match out.last().map(String::as_str) {
                Some("..") => out.push("..".to_string()),
                Some(_) => {
                    out.pop();
                }
                None if absolute => {}
                None => out.push("..".to_string()),
            },
            name => out.push(name.to_string()),
        }
    }
    out
}

pub(crate) fn normalize(path: &str) -> String {
    let parts = components(path);
    match (is_absolute(path), parts.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{}", parts.join("/")),
        (false, true) => ".".to_string(),
        (false, false) => parts.join("/"),
    }
}

pub(crate) fn join(parts: &[String]) -> String {
    let joined = parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("/");
    normalize(&joined)
}

fn trimmed(path: &str) -> &str {
    let stripped = path.trim_end_matches('/');
    if stripped.is_empty() && is_absolute(path) {
        "/"
    } else {
        stripped
    }
}

pub(crate) fn base(path: &str) -> String {
    let path = trimmed(path);
    if path == "/" {
        return String::new();
    }
    path.rsplit('/').next().unwrap_or_default().to_string()
}

pub(crate) fn dir(path: &str) -> String {
    let path = trimmed(path);
    match path.rfind('/') {
        None => ".".to_string(),
        Some(_) if path == "/" => "/".to_string(),
        Some(i) => {
            let parent = trimmed(&path[..i]);
            if parent.is_empty() {
                if is_absolute(path) {
                    "/".to_string()
                } else {
                    ".".to_string()
                }
            } else {
                parent.to_string()
            }
        }
    }
}

pub(crate) fn ext(path: &str) -> String {
    let name = base(path);
    match name.rfind('.') {
        Some(i) if i > 0 && name != ".." => name[i..].to_string(),
        _ => String::new(),
    }
}

pub(crate) fn relative(from: &str, to: &str) -> Result<String, String> {
    if is_absolute(from) != is_absolute(to) {
        return Err("both paths must be absolute or both relative".to_string());
    }
    let from = components(from);
    let to = components(to);
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if from[shared..].iter().any(|c| c == "..") {
        return Err("the start path climbs above the shared base, so the route is unknown".to_string());
    }
    let mut steps: Vec<&str> = vec![".."; from.len() - shared];
    steps.extend(to[shared..].iter().map(String::as_str));
    Ok(if steps.is_empty() { ".".to_string() } else { steps.join("/") })
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
        Ok(Value::String(join(&parts)))
    }
}

/// os/path-normalize
pub struct OsPathNormalizeOp;
impl Operator for OsPathNormalizeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-normalize", == 1);
        Ok(Value::String(normalize(&path_arg(&args[0], ctx, "os/path-normalize")?)))
    }
}

/// os/path-base - the last component
pub struct OsPathBaseOp;
impl Operator for OsPathBaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-base", == 1);
        Ok(Value::String(base(&path_arg(&args[0], ctx, "os/path-base")?)))
    }
}

/// os/path-dir - everything before the last component
pub struct OsPathDirOp;
impl Operator for OsPathDirOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-dir", == 1);
        Ok(Value::String(dir(&path_arg(&args[0], ctx, "os/path-dir")?)))
    }
}

/// os/path-ext - the last component's extension, with its dot
pub struct OsPathExtOp;
impl Operator for OsPathExtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-ext", == 1);
        Ok(Value::String(ext(&path_arg(&args[0], ctx, "os/path-ext")?)))
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
            .map(Value::String)
            .map_err(|message| invalid("os/path-relative", message))
    }
}

/// os/path-is-absolute
pub struct OsPathIsAbsoluteOp;
impl Operator for OsPathIsAbsoluteOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "os/path-is-absolute", == 1);
        Ok(Value::Bool(is_absolute(&path_arg(&args[0], ctx, "os/path-is-absolute")?)))
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
            Ok(Value::String(s)) => s,
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
    fn the_operators_are_wired_by_name() {
        assert_eq!(text(serde_json::json!(["os/path-join", "/a", "b", "../c"])), "/a/c");
        assert_eq!(text(serde_json::json!(["os/path-normalize", "a//b/../c/"])), "a/c");
        assert_eq!(text(serde_json::json!(["os/path-base", "/a/b.txt"])), "b.txt");
        assert_eq!(text(serde_json::json!(["os/path-dir", "/a/b.txt"])), "/a");
        assert_eq!(text(serde_json::json!(["os/path-ext", "/a/b.txt"])), ".txt");
        assert_eq!(text(serde_json::json!(["os/path-relative", "/a/b", "/a/c"])), "../c");
        assert_eq!(eval(serde_json::json!(["os/path-is-absolute", "/a"])).ok(), Some(Value::Bool(true)));
        assert_eq!(eval(serde_json::json!(["os/path-is-absolute", "a"])).ok(), Some(Value::Bool(false)));
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
