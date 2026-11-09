use super::*;
use crate::effects::response::MountSeed;
use crate::trait_owners::TraitOwners;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The circuit a client mounts for one page: which page the kernel resolved, what mounts, and the owning
/// orbital and linked entity of each trait the page binds (EXEC-ENDPOINT-3).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCircuit {
    pub route: Option<String>,
    pub page: Option<String>,
    /// The resolved page's declared nav `@label`; absent when it declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub not_found: bool,
    pub mounts: Vec<MountSeed>,
    pub embedded: Vec<String>,
    pub embedded_by: Vec<String>,
    pub orbital_by_trait: BTreeMap<String, String>,
    pub linked_entity_by_trait: BTreeMap<String, String>,
    /// The traits the page binds: its own, then the ones it embeds.
    pub traits: Vec<String>,
    /// The entities the page needs: each bound trait's linked entity and every entity reached through declared
    /// relation fields of those, transitively, sorted (EXEC-SEED-7).
    pub entities: Vec<String>,
    /// The declared route pages whose pattern matches a strict prefix of the route, shallowest first (the nav-stack cold-load seed).
    pub ancestors: Vec<PageAncestor>,
}

/// One ancestor of a route: the concrete prefix and the page declared for it.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageAncestor {
    pub path: String,
    pub page: String,
    /// The ancestor page's declared nav `@label`; absent when it declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

struct RoutePage {
    name: String,
    path: String,
    traits: Vec<String>,
    label: Option<String>,
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
                    label: page.label.clone(),
                    is_initial: page.is_initial == Some(true),
                    is_not_found: page.is_not_found == Some(true),
                });
            }
        });
        pages
    }

    /// Every trait reachable from `roots` through `@trait.X` (EXEC-EVAL-9), transitively, in first-met order;
    /// a root appears only when some trait in the walk references it.
    pub(super) fn trait_refs_from(&self, roots: &[String]) -> Vec<String> {
        let refs = self.registry.embedded_trait_refs.read();
        let mut walked: Vec<String> = roots.to_vec();
        let mut referenced: Vec<String> = Vec::new();
        let mut at = 0;
        while at < walked.len() {
            if let Some(children) = refs.get(walked[at].as_str()).map(Arc::clone) {
                for child in children.iter().cloned() {
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

    /// The INIT payload a page mount of `route` carries: the route params of the page it resolves to (EXEC-ENDPOINT-3),
    /// as a client's `page_mount_payload`; `null` when the page has none or no page matches.
    pub fn route_mount_payload(&self, route: &str) -> Value {
        let pages = self.route_pages();
        orbital_core_base::route::best_path_match(route, pages.iter().map(|p| p.path.as_str()))
            .and_then(|pattern| orbital_core_base::route::route_params(pattern, route))
            .filter(|params| !params.is_empty())
            .map_or(Value::Null, Value::object)
    }

    /// Which page a client shows and what mounts for it: the named page, else the most specific page a route matches,
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
            (None, Some(route)) => orbital_core_base::route::best_path_match(route, pages.iter().map(|p| p.path.as_str()))
                .and_then(|best| pages.iter().find(|p| p.path == best))
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
                label: None,
                not_found,
                mounts: Vec::new(),
                embedded: Vec::new(),
                embedded_by: Vec::new(),
                orbital_by_trait: BTreeMap::new(),
                linked_entity_by_trait: BTreeMap::new(),
                traits: Vec::new(),
                entities: Vec::new(),
                ancestors: self.route_ancestors(route, &pages),
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
        let traits: Vec<String> = bound.iter().map(|t| (*t).clone()).collect();
        let entities = self.entities_of(&traits);
        Ok(PageCircuit {
            route: Some(route.map_or_else(|| resolved.path.clone(), str::to_string)),
            page: Some(resolved.name.clone()),
            label: resolved.label.clone(),
            not_found,
            mounts: self.route_mounts(&resolved.path),
            embedded,
            embedded_by,
            orbital_by_trait,
            linked_entity_by_trait,
            traits,
            entities,
            ancestors: self.route_ancestors(route, &pages),
        })
    }

    fn route_ancestors(&self, route: Option<&str>, pages: &[RoutePage]) -> Vec<PageAncestor> {
        let Some(route) = route else { return Vec::new() };
        let patterns: Vec<&str> = pages.iter().map(|p| p.path.as_str()).collect();
        orbital_core_base::route::ancestor_matches(route, &patterns)
            .into_iter()
            .filter_map(|(path, pattern)| {
                pages.iter().find(|p| p.path == pattern).map(|p| PageAncestor { path, page: p.name.clone(), label: p.label.clone() })
            })
            .collect()
    }

    /// The entities `traits` need: each one's linked entity and every entity reached through the declared relation
    /// fields of those, transitively (a cycle ends where it closes), sorted.
    pub(super) fn entities_of(&self, traits: &[String]) -> Vec<String> {
        let mut pending: Vec<String> = traits.iter().filter_map(|t| self.linked_entity_of(t)).collect();
        let defs = self.registry.entity_definitions.read();
        let mut reached: BTreeSet<String> = BTreeSet::new();
        while let Some(entity) = pending.pop() {
            if !reached.insert(entity.clone()) {
                continue;
            }
            pending.extend(crate::relations::relation_target_entities(&defs, &entity));
        }
        reached.into_iter().collect()
    }
}
