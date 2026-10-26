//! A page renders under the locale its path names, else the first declared
//! one; an explicit translation group may hold each locale only once.

use orbital_core_base::messages::{duplicate_translation_locales, locale_of_path};

fn locales() -> Vec<String> {
    vec!["en".into(), "ar".into(), "sl".into()]
}

#[test]
fn a_path_renders_under_its_prefix_locale_else_the_first() {
    let l = locales();
    assert_eq!(locale_of_path(&l, "/ar/blog/x"), Some("ar"));
    assert_eq!(locale_of_path(&l, "/blog/x"), Some("en"));
    assert_eq!(locale_of_path(&l, "/"), Some("en"));
    assert_eq!(locale_of_path(&[], "/ar"), None);
}

#[test]
fn a_group_holding_one_locale_twice_is_reported_with_its_paths() {
    let found = duplicate_translation_locales(
        [
            ("/blog/x", Some("blog/x")),
            ("/blog/x-copy", Some("blog/x")),
            ("/ar/blog/x", Some("blog/x")),
        ],
        &locales(),
    );
    assert_eq!(
        found,
        vec![(
            "blog/x".to_string(),
            "en".to_string(),
            vec!["/blog/x".to_string(), "/blog/x-copy".to_string()]
        )]
    );
}

#[test]
fn control_one_page_per_locale_and_ungrouped_pages_are_fine() {
    let found = duplicate_translation_locales(
        [
            ("/blog/x", Some("blog/x")),
            ("/ar/blog/x", Some("blog/x")),
            ("/a", None),
            ("/b", None),
        ],
        &locales(),
    );
    assert!(found.is_empty(), "{found:?}");
}
