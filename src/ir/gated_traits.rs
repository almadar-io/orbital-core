//! Which traits only gated pages render. The web `PageAccessHost` mounts an
//! `access: authenticated` page's circuit only once its viewer is authenticated;
//! a native host mirrors that by INITing these traits when that page first
//! composes for an authenticated viewer, not at app start.

use super::OirModule;
use crate::schema::PageAccess;
use std::collections::{BTreeMap, BTreeSet};

/// Per gated page path, the traits it binds that no public page binds.
/// A trait bound by any public page, or by no page at all, stays eager.
pub fn gated_page_traits(module: &OirModule) -> BTreeMap<String, Vec<String>> {
    let gated = |access: &Option<PageAccess>| *access == Some(PageAccess::Authenticated);
    let public: BTreeSet<&str> = module
        .pages
        .iter()
        .filter(|p| !gated(&p.access))
        .flat_map(|p| p.traits.iter().map(|t| t.trait_name.as_str()))
        .collect();
    module
        .pages
        .iter()
        .filter(|p| gated(&p.access))
        .map(|p| {
            let mut traits: Vec<String> = Vec::new();
            for t in &p.traits {
                if !public.contains(t.trait_name.as_str()) && !traits.contains(&t.trait_name) {
                    traits.push(t.trait_name.clone());
                }
            }
            (p.path.clone(), traits)
        })
        .filter(|(_, traits)| !traits.is_empty())
        .collect()
}

/// Every trait that only gated pages bind.
pub fn gated_only_traits(module: &OirModule) -> BTreeSet<String> {
    gated_page_traits(module).into_values().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{OirPage, OirTraitBinding};

    fn bind(name: &str) -> OirTraitBinding {
        OirTraitBinding {
            trait_name: name.to_string(),
            trait_id: None,
            config: None,
        }
    }

    fn page(path: &str, access: Option<PageAccess>, traits: &[&str]) -> OirPage {
        let mut p = OirPage::new(path.trim_start_matches('/'), path);
        p.access = access;
        p.traits = traits.iter().map(|t| bind(t)).collect();
        p
    }

    #[test]
    fn a_trait_only_a_gated_page_binds_is_gated_under_that_page() {
        let mut module = OirModule::new("T", "1.0.0");
        module.pages.push(page("/", None, &["Home", "Layout"]));
        module.pages.push(page(
            "/account",
            Some(PageAccess::Authenticated),
            &["Account", "Layout"],
        ));
        let map = gated_page_traits(&module);
        assert_eq!(map.get("/account"), Some(&vec!["Account".to_string()]));
        assert_eq!(
            gated_only_traits(&module).into_iter().collect::<Vec<_>>(),
            vec!["Account"]
        );
    }

    #[test]
    fn control_a_trait_a_public_page_also_binds_stays_eager() {
        let mut module = OirModule::new("T", "1.0.0");
        module
            .pages
            .push(page("/feed", Some(PageAccess::Public), &["Feed"]));
        module
            .pages
            .push(page("/mine", Some(PageAccess::Authenticated), &["Feed"]));
        assert!(gated_page_traits(&module).is_empty());
    }

    #[test]
    fn control_an_app_with_no_gated_page_gates_nothing() {
        let mut module = OirModule::new("T", "1.0.0");
        module.pages.push(page("/", None, &["Home"]));
        assert!(gated_only_traits(&module).is_empty());
    }
}
