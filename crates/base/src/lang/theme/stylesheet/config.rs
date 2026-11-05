#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::{ThemeDefinition, ThemeTokens};
use crate::str_utils::capitalize_first;



/// One theme as the web client's `ClientManifest` lists it (`@almadar/core` `ClientTheme`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTheme {
    pub name: String,
    pub display_name: String,
    pub has_light_mode: bool,
    pub has_dark_mode: bool,
}

/// The program's themes, its default theme and the default mode, as the client reads them. A
/// `default_theme` key ending in `-dark` or `-light` sets the mode; without one the first theme is
/// the default, in light mode; a program with no themes has no default.
pub fn client_themes(themes: &[ThemeDefinition], default_theme: Option<&str>) -> (Vec<ClientTheme>, Option<String>, &'static str) {
    let listed = themes
        .iter()
        .map(|theme| ClientTheme {
            name: theme.name.clone(),
            display_name: theme.display_name.clone().unwrap_or_else(|| capitalize_first(&theme.name)),
            has_light_mode: !theme_tokens_empty(&theme.tokens),
            has_dark_mode: theme.variants.as_ref().is_some_and(|v| v.contains_key("dark")),
        })
        .collect();
    let (default, mode) = match default_theme {
        Some(key) => match key.strip_suffix("-dark") {
            Some(base) => (Some(base.to_string()), "dark"),
            None => (Some(key.strip_suffix("-light").unwrap_or(key).to_string()), "light"),
        },
        None => (themes.first().map(|t| t.name.clone()), "light"),
    };
    (listed, default, mode)
}

fn theme_tokens_empty(t: &ThemeTokens) -> bool {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.is_empty()))
        .unwrap_or(true)
}
