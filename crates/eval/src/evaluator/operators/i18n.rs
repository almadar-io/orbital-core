//! `i18n/t`: the active locale's message for a catalog key (keys arrive
//! qualified with their behavior's name — see `crate::messages`).

use super::macros::*;
use super::*;

pub struct I18nTOp;
impl Operator for I18nTOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "i18n/t", >= 1);
        let key = eval_expr(&args[0], ctx)?.to_string_value();
        let Some(message) = ctx.message(&key) else {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "no `{}` message for `{key}` — `orb validate` checks every key against every declared locale",
                ctx.locale.to_string_value()
            ))));
        };
        let mut out = message.to_string();
        if let Some(params) = args.get(1) {
            if let Value::Object(map) = eval_expr(params, ctx)? {
                for (name, value) in &map {
                    out = out.replace(&format!("{{{{{name}}}}}"), &value.to_string_value());
                }
            }
        }
        Ok(Value::String(out))
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
            .locale(Value::String(locale.into()))
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

    #[test]
    fn resolves_the_active_locales_message() {
        let c = ctx("ar", &[("home:hero.title", "مرحبا")]);
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:hero.title"])), &c).unwrap();
        assert_eq!(v, Value::String("مرحبا".into()));
    }

    #[test]
    fn fills_placeholders_from_params() {
        let c = ctx("en", &[("home:posts.count", "{{n}} posts by {{who}}")]);
        let v = eval_expr(
            &call(serde_json::json!(["i18n/t", "home:posts.count", { "n": 3, "who": "Osama" }])),
            &c,
        )
        .unwrap();
        assert_eq!(v, Value::String("3 posts by Osama".into()));
    }

    #[test]
    fn control_another_locales_catalog_gives_another_message() {
        let en = ctx("en", &[("home:hero.title", "Hello")]);
        let v = eval_expr(&call(serde_json::json!(["i18n/t", "home:hero.title"])), &en).unwrap();
        assert_eq!(v, Value::String("Hello".into()));
    }

    #[test]
    fn a_missing_key_is_an_error_not_a_fallback() {
        let c = ctx("sl", &[]);
        assert!(eval_expr(&call(serde_json::json!(["i18n/t", "home:hero.title"])), &c).is_err());
    }

    #[test]
    fn the_locale_binding_reads_the_active_locale() {
        let c = ctx("sl", &[]);
        let v = eval_expr(&call(serde_json::json!("@locale")), &c).unwrap();
        assert_eq!(v, Value::String("sl".into()));
    }
}
