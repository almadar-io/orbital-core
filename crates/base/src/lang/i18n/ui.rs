//! The UI's own message catalogs (`ui/{en,ar,sl}.json`): the built-in labels every host paints when the program's
//! catalog does not carry a key. `renderers/web/src/core/locales/*.json` are byte copies of these files.

use std::collections::BTreeMap;
use std::sync::OnceLock;

type Catalog = BTreeMap<String, String>;

fn parse(source: &str) -> Catalog {
    let json: serde_json::Value = serde_json::from_str(source).expect("built-in ui catalog");
    json.as_object()
        .expect("ui catalog is an object")
        .iter()
        .filter(|(key, _)| !key.starts_with('$'))
        .filter_map(|(key, value)| value.as_str().map(|text| (key.clone(), text.to_string())))
        .collect()
}

fn catalogs() -> &'static BTreeMap<&'static str, Catalog> {
    static UI: OnceLock<BTreeMap<&'static str, Catalog>> = OnceLock::new();
    UI.get_or_init(|| {
        BTreeMap::from([
            ("en", parse(include_str!("ui/en.json"))),
            ("ar", parse(include_str!("ui/ar.json"))),
            ("sl", parse(include_str!("ui/sl.json"))),
        ])
    })
}

/// The UI catalog's message for `key` in `locale`, else English, else the key itself.
pub fn ui_message(locale: &str, key: &str) -> String {
    let all = catalogs();
    all.get(locale)
        .and_then(|catalog| catalog.get(key))
        .or_else(|| all["en"].get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// `message` with every `{{name}}` replaced by its value in `params`.
pub fn interpolate(message: &str, params: &[(&str, String)]) -> String {
    params
        .iter()
        .fold(message.to_string(), |text, (name, value)| {
            text.replace(&format!("{{{{{name}}}}}"), value)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_locale_catalog_answers_then_english_then_the_key() {
        assert_eq!(ui_message("en", "common.save"), "Save");
        assert_ne!(
            ui_message("ar", "common.save"),
            "Save",
            "ar paints its own text"
        );
        assert_eq!(
            ui_message("ar", "common.save"),
            parse(include_str!("ui/ar.json"))["common.save"]
        );
        assert_eq!(
            ui_message("fr", "common.save"),
            "Save",
            "control: an unshipped locale falls to en"
        );
        assert_eq!(ui_message("ar", "no.such.key"), "no.such.key");
    }

    #[test]
    fn interpolation_replaces_each_named_slot() {
        assert_eq!(
            interpolate(
                "Showing {{count}} of {{total}}",
                &[("count", "5".into()), ("total", "20".into())]
            ),
            "Showing 5 of 20"
        );
    }
}
