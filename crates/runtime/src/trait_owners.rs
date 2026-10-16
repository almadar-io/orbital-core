//! Trait name -> owning orbital, the one routing index every host that addresses a server leg
//! per orbital shares (`orbital-ffi`'s wire, `orbital-client`'s `ClientKernel`).

use super::kernel::RuntimeKernel;
use std::collections::{BTreeMap, BTreeSet};

/// Trait name -> every orbital that declares it, read from the kernel's own registered view
/// (`list_orbitals` / `get_orbital`).
#[derive(Debug, Clone, Default)]
pub struct TraitOwners(BTreeMap<String, BTreeSet<String>>);

impl TraitOwners {
    pub fn from_kernel(kernel: &RuntimeKernel) -> Self {
        let mut owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for info in kernel.list_orbitals() {
            if let Some(detail) = kernel.get_orbital(&info.name) {
                for trait_name in detail.traits {
                    owners
                        .entry(trait_name)
                        .or_default()
                        .insert(detail.name.clone());
                }
            }
        }
        Self(owners)
    }

    /// The single orbital declaring `trait_name`; unknown and ambiguous traits are errors (an
    /// ambiguous one names every candidate, none is picked).
    pub fn owning_orbital(&self, trait_name: &str) -> Result<&str, String> {
        match self.0.get(trait_name) {
            None => Err(format!("unknown trait: {trait_name}")),
            Some(orbitals) if orbitals.len() == 1 => Ok(orbitals
                .iter()
                .next()
                .map(String::as_str)
                .unwrap_or_default()),
            Some(orbitals) => Err(format!(
                "ambiguous trait {trait_name}: declared by orbitals {}",
                orbitals.iter().cloned().collect::<Vec<_>>().join(", ")
            )),
        }
    }

    /// Attribution label for traces: the owner, or every candidate joined by `, ` when ambiguous
    /// (never one of them alone).
    pub fn label(&self, trait_name: &str) -> String {
        self.0
            .get(trait_name)
            .map(|o| o.iter().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &BTreeSet<String>)> {
        self.0.iter()
    }
}
