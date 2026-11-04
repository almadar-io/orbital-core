//! Row action routes: a literal path, or a quoted single-row lambda `(quote (fn row <body>))`.
//!
//! The lambda IR is `["fn", "<param>", <body>]` with a bare parameter name; the quoted form wraps it as
//! `["quote", "<json>"]` (see `quote`). One parser serves the compiler's check and the kernel's evaluation.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::quote::{decode_quote_body, quote_body_of};
use super::sexpr::SExpression;

/// A parsed single-parameter row lambda.
#[derive(Debug, Clone, PartialEq)]
pub struct RowLambda {
    pub param: String,
    pub body: SExpression,
}

/// A parsed `navigatesTo`.
#[derive(Debug, Clone, PartialEq)]
pub enum RowRoute {
    Literal(String),
    Lambda(RowLambda),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RowRouteError {
    #[error("a row lambda is the array `[\"fn\", \"<param>\", <body>]`")]
    NotALambda,
    #[error("a row lambda takes exactly one parameter and one body (`[\"fn\", \"<param>\", <body>]`), found {found} elements")]
    Arity { found: usize },
    #[error("a row lambda starts with `fn`")]
    NotFn,
    #[error("a row lambda parameter is a bare, non-empty name (no `@`)")]
    BadParam,
    #[error("a quoted row lambda body is not JSON")]
    QuoteBodyNotJson,
    #[error("a route is a literal path or a quoted row lambda")]
    NotARoute,
    #[error("route `{0}` must start with `/`")]
    NotAPath(String),
    #[error("route `{0}` holds `{{` or `}}`; a templated route is a quoted row lambda")]
    Template(String),
}

/// A route literal: a path starting with `/` that holds no `{` or `}`.
pub fn parse_route_literal(route: &str) -> Result<&str, RowRouteError> {
    if route.contains(['{', '}']) {
        return Err(RowRouteError::Template(route.to_string()));
    }
    if !route.starts_with('/') {
        return Err(RowRouteError::NotAPath(route.to_string()));
    }
    Ok(route)
}

/// The lowered lambda `["fn", "<param>", <body>]`.
pub fn parse_row_lambda(expr: &SExpression) -> Result<RowLambda, RowRouteError> {
    let SExpression::Array(parts) = expr else {
        return Err(RowRouteError::NotALambda);
    };
    if parts.first().and_then(SExpression::as_str) != Some("fn") {
        return Err(RowRouteError::NotFn);
    }
    if parts.len() != 3 {
        return Err(RowRouteError::Arity { found: parts.len() });
    }
    match parts[1].as_str() {
        Some(param) if !param.is_empty() && !param.contains('@') => Ok(RowLambda {
            param: param.to_string(),
            body: parts[2].clone(),
        }),
        _ => Err(RowRouteError::BadParam),
    }
}

/// `["quote", "<json>"]` holding a row lambda.
pub fn parse_quoted_row_lambda(expr: &SExpression) -> Result<RowLambda, RowRouteError> {
    let body = quote_body_of(expr).ok_or(RowRouteError::NotARoute)?;
    let decoded = decode_quote_body(body).ok_or(RowRouteError::QuoteBodyNotJson)?;
    parse_row_lambda(&decoded)
}

/// A `navigatesTo`: a literal path string, or a quoted row lambda.
pub fn parse_row_route(expr: &SExpression) -> Result<RowRoute, RowRouteError> {
    match expr {
        SExpression::String(route) => parse_route_literal(route).map(|route| RowRoute::Literal(route.to_string())),
        other => parse_quoted_row_lambda(other).map(RowRoute::Lambda),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> SExpression {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn route_literal_accepts_paths_and_rejects_templates_in_any_spacing() {
        assert_eq!(parse_route_literal("/entries"), Ok("/entries"));
        for template in ["/x/{{row.id}}", "/x/{{ row.id }}", "/x/{{item.id}}", "/x/{id}"] {
            assert!(matches!(parse_route_literal(template), Err(RowRouteError::Template(_))), "{template}");
        }
        assert_eq!(parse_route_literal("entries"), Err(RowRouteError::NotAPath("entries".into())));
    }

    #[test]
    fn row_lambda_parses_one_spelling_and_names_each_failure() {
        let lambda = parse_row_lambda(&parse(r#"["fn","row",["str/concat","/x/","@row.id"]]"#)).unwrap();
        assert_eq!(lambda.param, "row");
        assert_eq!(parse_row_lambda(&parse(r#""/x""#)), Err(RowRouteError::NotALambda));
        assert_eq!(parse_row_lambda(&parse(r#"["str/concat","a"]"#)), Err(RowRouteError::NotFn));
        assert_eq!(parse_row_lambda(&parse(r#"["fn","a","b","/x"]"#)), Err(RowRouteError::Arity { found: 4 }));
        assert_eq!(parse_row_lambda(&parse(r#"["fn",["row"],"/x"]"#)), Err(RowRouteError::BadParam));
        assert_eq!(parse_row_lambda(&parse(r#"["fn","@row","/x"]"#)), Err(RowRouteError::BadParam));
        assert_eq!(parse_row_lambda(&parse(r#"["fn","","/x"]"#)), Err(RowRouteError::BadParam));
    }

    #[test]
    fn row_route_reads_literal_and_quoted_lambda() {
        let quoted = parse(r#"["quote","[\"fn\",\"row\",[\"str/concat\",\"/x/\",\"\\u0040row.id\"]]"]"#);
        assert!(matches!(parse_row_route(&quoted), Ok(RowRoute::Lambda(_))));
        assert_eq!(parse_row_route(&parse(r#""/x""#)), Ok(RowRoute::Literal("/x".into())));
        assert_eq!(parse_row_route(&parse(r#"["quote","not json"]"#)), Err(RowRouteError::QuoteBodyNotJson));
        assert_eq!(parse_row_route(&parse(r#"["fn","row","/x"]"#)), Err(RowRouteError::NotARoute));
        assert_eq!(parse_row_route(&parse(r#"["quote","[\"fn\",\"row\"]"]"#)), Err(RowRouteError::Arity { found: 2 }));
    }
}
