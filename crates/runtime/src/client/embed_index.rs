//! Runtime inverse index over `TraitDefinition.trait_embed_ids` (§158).
//!
//! `trait_embed_ids` is a host-owned map of embed token NAME -> embedded child
//! trait's stable `id`. The kernel needs the opposite direction at dispatch
//! time — "which host(s), if any, embed trait X" — to bubble a UI-originated
//! emit up the embed scope chain the way `TraitScopeProvider`
//! (R-EMBEDDED-EMIT-BUBBLE) does on the JS path. This module builds that
//! inverse once per schema registration; nothing here mutates dispatch state.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::schema::types::TraitDefinition;
use std::collections::{HashMap, HashSet};

/// Trait name -> innermost-first chain of ancestor trait names (its direct
/// embedding host, then that host's own host, and so on).
#[derive(Debug, Default, Clone)]
pub struct EmbedIndex {
    host_chain: HashMap<String, Vec<String>>,
}

impl EmbedIndex {
    /// Build the index from the kernel's full accumulated trait definitions.
    ///
    /// `trait_embed_ids` values are child STABLE IDS, not names, so this
    /// first inverts `id -> name` over every known definition, then inverts
    /// `host -> {token: child_id}` into `child_name -> host_name` (first host
    /// in sorted host-name order wins when a child is embedded by more than
    /// one host — same first-referrer-per-hop contract as the compiler's
    /// `embed_graph::embedder_chain`), then walks each trait's transitive
    /// chain innermost-first with a visited guard bounded by the map size.
    pub fn build<T: std::borrow::Borrow<TraitDefinition>>(
        trait_definitions: &HashMap<String, T>,
    ) -> Self {
        let mut id_to_name: HashMap<&str, &str> = HashMap::new();
        for def in trait_definitions.values().map(std::borrow::Borrow::borrow) {
            if let Some(id) = &def.id {
                id_to_name.insert(id.as_str(), def.name.as_str());
            }
        }

        let mut host_names: Vec<&str> = trait_definitions.keys().map(String::as_str).collect();
        host_names.sort_unstable();

        let mut direct_host: HashMap<String, String> = HashMap::new();
        for host_name in host_names {
            let Some(embed_ids) = trait_definitions
                .get(host_name)
                .and_then(|def| def.borrow().trait_embed_ids.as_ref())
            else {
                continue;
            };
            let mut child_ids: Vec<&String> = embed_ids.values().collect();
            child_ids.sort_unstable();
            for child_id in child_ids {
                let Some(&child_name) = id_to_name.get(child_id.as_str()) else {
                    // Unknown child id (e.g. dangling reference) — skip.
                    continue;
                };
                direct_host
                    .entry(child_name.to_string())
                    .or_insert_with(|| host_name.to_string());
            }
        }

        let bound = trait_definitions.len().max(1);
        let mut host_chain: HashMap<String, Vec<String>> = HashMap::new();
        for trait_name in trait_definitions.keys() {
            let mut chain = Vec::new();
            let mut visited: HashSet<&str> = HashSet::new();
            let mut current = trait_name.as_str();
            while chain.len() < bound && visited.insert(current) {
                let Some(parent) = direct_host.get(current) else {
                    break;
                };
                chain.push(parent.clone());
                current = parent.as_str();
            }
            host_chain.insert(trait_name.clone(), chain);
        }

        Self { host_chain }
    }

    /// Innermost-first ancestor chain for `trait_name` (its direct host
    /// first, then that host's host, ...). Empty when the trait is not
    /// embedded by anything.
    pub fn ancestors(&self, trait_name: &str) -> &[String] {
        self.host_chain
            .get(trait_name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
