//! Type identity across composed behaviors (R13, G-ORB-468): a behavior's local type `T` is the one type
//! `<Behavior>::<T>` in a composing program, so two behaviors may each declare `T` without collision.

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

/// The separator between a behavior and a type it declares.
pub const TYPE_QUALIFIER: &str = "::";

/// The program-wide name of `local`, a type that `behavior` declares.
pub fn qualified_type_name(behavior: &str, local: &str) -> String {
    format!("{behavior}{TYPE_QUALIFIER}{local}")
}

/// A type name split into its declaring behavior (when qualified) and the name the author wrote.
pub fn split_qualified_type_name(name: &str) -> (Option<&str>, &str) {
    match name.rsplit_once(TYPE_QUALIFIER) {
        Some((behavior, local)) if !behavior.is_empty() && !local.is_empty() => {
            (Some(behavior), local)
        }
        _ => (None, name),
    }
}

/// The name an author wrote: findings and printed `.lolo` use it.
pub fn short_type_name(name: &str) -> &str {
    split_qualified_type_name(name).1
}

/// A type name as a finding words it: the short name, and the behavior that declares it in words.
pub fn describe_type_name(name: &str) -> String {
    match split_qualified_type_name(name) {
        (Some(behavior), local) => format!("{local} (declared by behavior {behavior})"),
        (None, local) => local.to_string(),
    }
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '-'
}

/// `text`, a declared type's spelling (`Name`, `[Name]`, `Map<K,V>`, `Name?`, a tuple), with every type name
/// `rename` answers for replaced; the structure around the names is kept as written.
pub fn rename_type_names(text: &str, rename: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if !is_name_char(c) {
            out.push(c);
            continue;
        }
        let mut end = start + c.len_utf8();
        while let Some(&(at, next)) = chars.peek() {
            if is_name_char(next) {
                end = at + next.len_utf8();
                chars.next();
            } else if next == ':'
                && text[at..].starts_with(TYPE_QUALIFIER)
                && text[at + 2..].chars().next().is_some_and(is_name_char)
            {
                chars.next();
                chars.next();
                end = at + 2;
            } else {
                break;
            }
        }
        let word = &text[start..end];
        match rename(word) {
            Some(renamed) => out.push_str(&renamed),
            None => out.push_str(word),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_type_is_qualified_by_its_behavior_and_splits_back() {
        let name = qualified_type_name("std-orb-complete", "CoreManifest");
        assert_eq!(name, "std-orb-complete::CoreManifest");
        assert_eq!(
            split_qualified_type_name(&name),
            (Some("std-orb-complete"), "CoreManifest")
        );
        assert_eq!(short_type_name(&name), "CoreManifest");
    }

    #[test]
    fn a_finding_words_a_qualified_name_without_the_qualifier_spelling() {
        assert_eq!(describe_type_name("B::Item"), "Item (declared by behavior B)");
        assert_eq!(describe_type_name("Item"), "Item");
    }

    #[test]
    fn an_unqualified_or_malformed_name_has_no_behavior() {
        for name in ["CoreManifest", "::X", "B::", ""] {
            assert_eq!(split_qualified_type_name(name).0, None, "{name}");
            assert_eq!(short_type_name(name), name);
        }
    }

    #[test]
    fn renaming_replaces_whole_names_inside_every_type_spelling() {
        let to = |name: &str| (name == "Item").then(|| "B::Item".to_string());
        assert_eq!(rename_type_names("Item", &to), "B::Item");
        assert_eq!(rename_type_names("[Item]", &to), "[B::Item]");
        assert_eq!(rename_type_names("Item?", &to), "B::Item?");
        assert_eq!(
            rename_type_names("Map<string,Item>", &to),
            "Map<string,B::Item>"
        );
        assert_eq!(
            rename_type_names("[Item, number]", &to),
            "[B::Item, number]"
        );
    }

    #[test]
    fn control_renaming_leaves_longer_names_primitives_and_other_qualifiers_alone() {
        let to = |name: &str| (name == "Item").then(|| "B::Item".to_string());
        for same in [
            "ItemRow",
            "string",
            "[Items]",
            "C::Item",
            "Item2",
            "Map<string,number>",
        ] {
            assert_eq!(rename_type_names(same, &to), same);
        }
    }
}
