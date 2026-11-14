//! `i18n/t`: the active locale's message for a catalog key (keys arrive
//! qualified with their behavior's name — see `crate::messages`).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

/// The `i18n/t` and `i18n/direction` option naming the locale to read instead of the active one; never a placeholder.
const LOCALE_OPTION: &str = "locale";

pub struct I18nTOp;
impl Operator for I18nTOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "i18n/t", >= 1);
        let key = eval_expr(&args[0], ctx)?.text()?.to_string();
        let params = match args.get(1) {
            Some(params) => Some(eval_expr(params, ctx)?),
            None => None,
        };
        let map = match &params {
            Some(params) => Some(params.as_object().ok_or_else(|| params.kind_error("object"))?),
            None => None,
        };
        let locale = match map.and_then(|m| m.get(LOCALE_OPTION)) {
            Some(locale) => Some(locale.text()?.to_string()),
            None => None,
        };
        let message = match &locale {
            Some(locale) => ctx.message_in(locale, &key),
            None => ctx.message(&key),
        };
        let Some(message) = message else {
            return Ok(Value::Null);
        };
        let mut out = message.to_string();
        for (name, value) in map.into_iter().flatten() {
            if name != LOCALE_OPTION {
                out = out.replace(&format!("{{{{{name}}}}}"), &value.to_string_value());
            }
        }
        Ok(Value::string(out))
    }
}

/// `i18n/direction`: the reading direction of the active locale (`rtl` or `ltr`), null until the host sets a locale.
pub struct I18nDirectionOp;
impl Operator for I18nDirectionOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.len() > 1 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "i18n/direction".to_string(),
                expected: 1,
                actual: args.len(),
            }));
        }
        if let Some(options) = args.first() {
            let options = eval_expr(options, ctx)?;
            let map = options.as_object().ok_or_else(|| options.kind_error("object"))?;
            if let Some(locale) = map.get(LOCALE_OPTION) {
                return Ok(Value::string(orbital_core_base::messages::locale_direction(locale.text()?)));
            }
        }
        Ok(match ctx.locale.as_str() {
            Some(locale) => Value::string(orbital_core_base::messages::locale_direction(locale)),
            None => Value::Null,
        })
    }
}

#[cfg(test)]
mod tests {
    use orbital_core_base::schema::sexpr::SExpression;
    use std::collections::HashMap;
    use {
        crate::evaluator::context::BindingContextBuilder, crate::evaluator::operators::eval_expr,
        orbital_core_base::value::Value,
    };

    fn ctx(locale: &str, messages: &[(&str, &str)]) -> crate::evaluator::context::BindingContext {
        BindingContextBuilder::new()
            .locale(Value::string(locale))
            .messages(
                messages
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
            )
            .build()
    }

    fn call(json: serde_json::Value) -> SExpression {
        serde_json::from_value(json).unwrap()
    }

    fn ctx_all(locale: &str, catalogs: &[(&str, &[(&str, &str)])]) -> crate::evaluator::context::BindingContext {
        let all: HashMap<String, HashMap<String, String>> = catalogs
            .iter()
            .map(|(l, m)| (l.to_string(), m.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()))
            .collect();
        let active = all.get(locale).cloned().unwrap_or_default();
        BindingContextBuilder::new()
            .locale(Value::string(locale))
            .messages(active)
            .catalogs(std::sync::Arc::new(all))
            .build()
    }

    fn two() -> crate::evaluator::context::BindingContext {
        ctx_all(
            "en",
            &[
                ("en", &[("home:hi", "Hello"), ("home:n", "{{n}} posts")]),
                ("ar", &[("home:hi", "مرحبا"), ("home:n", "{{n}} مقالات")]),
            ],
        )
    }

    #[test]
    fn a_named_locale_reads_that_locales_catalog() {
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:hi", { "locale": "ar" }])), &two()).unwrap();
        assert_eq!(v, Value::string("مرحبا"));
    }

    #[test]
    fn control_without_a_locale_the_active_catalog_answers() {
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:hi"])), &two()).unwrap();
        assert_eq!(v, Value::string("Hello"));
    }

    #[test]
    fn the_locale_option_is_not_a_placeholder_and_params_still_fill() {
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:n", { "locale": "ar", "n": 3 }])), &two()).unwrap();
        assert_eq!(v, Value::string("3 مقالات"));
    }

    #[test]
    fn edge_a_locale_with_no_catalog_answers_null_not_a_fallback() {
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:hi", { "locale": "sl" }])), &two()).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn edge_a_non_string_locale_is_a_type_mismatch() {
        assert!(eval_expr(&call(serde_json::json!(["i18n/t", "home:hi", { "locale": 3 }])), &two()).is_err());
    }

    #[test]
    fn the_direction_of_a_named_locale() {
        let c = two();
        let ar = eval_expr(&call(serde_json::json!(["i18n/direction", { "locale": "ar" }])), &c).unwrap();
        assert_eq!(ar, Value::string("rtl"));
        let active = eval_expr(&call(serde_json::json!(["i18n/direction"])), &c).unwrap();
        assert_eq!(active, Value::string("ltr"), "control: no locale reads the active one");
        assert!(eval_expr(&call(serde_json::json!(["i18n/direction", { "locale": 1 }])), &c).is_err());
    }

    #[test]
    fn the_direction_of_the_active_locale() {
        for (locale, dir) in [("ar", "rtl"), ("he", "rtl"), ("en", "ltr"), ("sl", "ltr")] {
            let v = eval_expr(&call(serde_json::json!(["i18n/direction"])), &ctx(locale, &[])).unwrap();
            assert_eq!(v, Value::string(dir), "{locale}");
        }
    }

    #[test]
    fn edge_with_no_locale_set_the_direction_is_null() {
        let none = BindingContextBuilder::new().build();
        assert_eq!(eval_expr(&call(serde_json::json!(["i18n/direction"])), &none).unwrap(), Value::Null);
    }

    #[test]
    fn resolves_the_active_locales_message() {
        let c = ctx("ar", &[("home:hero.title", "مرحبا")]);
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:hero.title"])), &c).unwrap();
        assert_eq!(v, Value::string("مرحبا"));
    }

    #[test]
    fn fills_placeholders_from_params() {
        let c = ctx("en", &[("home:posts.count", "{{n}} posts by {{who}}")]);
        let v = eval_expr(
            &call(serde_json::json!(["i18n/t", "home:posts.count", { "n": 3, "who": "Osama" }])),
            &c,
        )
        .unwrap();
        assert_eq!(v, Value::string("3 posts by Osama"));
    }

    #[test]
    fn control_another_locales_catalog_gives_another_message() {
        let en = ctx("en", &[("home:hero.title", "Hello")]);
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:hero.title"])), &en).unwrap();
        assert_eq!(v, Value::string("Hello"));
    }

    #[test]
    fn a_missing_key_answers_null_not_a_fallback() {
        let c = ctx("sl", &[]);
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:hero.title"])), &c).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn the_locale_binding_reads_the_active_locale() {
        let c = ctx("sl", &[]);
        let v = eval_expr(&call(serde_json::json!("@locale")), &c).unwrap();
        assert_eq!(v, Value::string("sl"));
    }

    #[test]
    fn a_non_string_key_or_non_object_params_are_type_mismatches() {
        let c = ctx("en", &[("k", "{{n}} items")]);
        assert!(eval_expr(&call(serde_json::json!(["i18n/t", null])), &c).is_err());
        assert!(eval_expr(&call(serde_json::json!(["i18n/t", "k", null])), &c).is_err());
        assert!(eval_expr(&call(serde_json::json!(["i18n/t", "k", [1]])), &c).is_err());
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "k", { "n": 2 }])), &c).unwrap();
        assert_eq!(v, Value::string("2 items"));
    }
}
