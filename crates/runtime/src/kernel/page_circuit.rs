use super::*;
use crate::effects::response::MountSeed;
use crate::trait_owners::TraitOwners;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The circuit a client mounts for one page: which page the kernel resolved, what mounts, and the owning
/// orbital and linked entity of each trait the page binds (EXEC-ENDPOINT-3).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCircuit {
    pub route: Option<String>,
    pub page: Option<String>,
    pub not_found: bool,
    pub mounts: Vec<MountSeed>,
    pub embedded: Vec<String>,
    pub embedded_by: Vec<String>,
    pub orbital_by_trait: BTreeMap<String, String>,
    pub linked_entity_by_trait: BTreeMap<String, String>,
}

struct RoutePage {
    name: String,
    path: String,
    traits: Vec<String>,
    is_initial: bool,
    is_not_found: bool,
}

impl RuntimeKernel {
    fn route_pages(&self) -> Vec<RoutePage> {
        let mut pages = Vec::new();
        self.each_inline_page(|page| {
            if page.is_route() {
                pages.push(RoutePage {
                    name: page.name.clone(),
                    path: page.path.clone(),
                    traits: page
                        .traits
                        .iter()
                        .flatten()
                        .filter_map(|t| t.trait_name())
                        .map(str::to_string)
                        .collect(),
                    is_initial: page.is_initial == Some(true),
                    is_not_found: page.is_not_found == Some(true),
                });
            }
        });
        pages
    }

    /// Every trait reachable from `roots` through `@trait.X` (EXEC-EVAL-9), transitively, in first-met order;
    /// a root appears only when some trait in the walk references it.
    fn trait_refs_from(&self, roots: &[String]) -> Vec<String> {
        let defs = self.registry.trait_definitions.read();
        let mut walked: Vec<String> = roots.to_vec();
        let mut referenced: Vec<String> = Vec::new();
        let mut at = 0;
        while at < walked.len() {
            if let Some(def) = defs.get(walked[at].as_str()) {
                for child in super::callsite_capture::embedded_trait_refs(def) {
                    if !referenced.contains(&child) {
                        referenced.push(child.clone());
                    }
                    if !walked.contains(&child) {
                        walked.push(child);
                    }
                }
            }
            at += 1;
        }
        referenced
    }

    /// Which page a client shows and what mounts for it: the named page, else the page a route matches,
    /// else the `[initial]` page, else the first route page in declaration order; an unmatched route resolves to the `[notFound]` page.
    pub fn page_circuit(
        &self,
        page: Option<&str>,
        route: Option<&str>,
    ) -> Result<PageCircuit, String> {
        let pages = self.route_pages();
        let mut not_found = false;
        let resolved = match (page, route) {
            (Some(name), _) => Some(
                pages
                    .iter()
                    .find(|p| p.name == name)
                    .ok_or_else(|| format!("unknown page: {name}"))?,
            ),
            (None, Some(route)) => pages
                .iter()
                .find(|p| orbital_core_base::route::path_matches_pattern(route, &p.path))
                .or_else(|| {
                    not_found = true;
                    pages.iter().find(|p| p.is_not_found)
                }),
            (None, None) => pages
                .iter()
                .find(|p| p.is_initial)
                .or_else(|| pages.first()),
        };
        let Some(resolved) = resolved else {
            return Ok(PageCircuit {
                route: route.map(str::to_string),
                page: None,
                not_found,
                mounts: Vec::new(),
                embedded: Vec::new(),
                embedded_by: Vec::new(),
                orbital_by_trait: BTreeMap::new(),
                linked_entity_by_trait: BTreeMap::new(),
            });
        };
        let owners = TraitOwners::from_kernel(self);
        let embedded_by = self.trait_refs_from(&resolved.traits);
        let embedded: Vec<String> = embedded_by
            .iter()
            .filter(|t| !resolved.traits.contains(t))
            .cloned()
            .collect();
        let bound: Vec<&String> = resolved.traits.iter().chain(&embedded).collect();
        let orbital_by_trait = bound
            .iter()
            .map(|t| {
                owners
                    .owning_orbital(t)
                    .map(|o| ((*t).clone(), o.to_string()))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let linked_entity_by_trait = bound
            .iter()
            .filter_map(|t| self.linked_entity_of(t).map(|e| ((*t).clone(), e)))
            .collect();
        Ok(PageCircuit {
            route: Some(route.map_or_else(|| resolved.path.clone(), str::to_string)),
            page: Some(resolved.name.clone()),
            not_found,
            mounts: self.route_mounts(&resolved.path),
            embedded,
            embedded_by,
            orbital_by_trait,
            linked_entity_by_trait,
        })
    }
}
