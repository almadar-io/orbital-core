//! Which traits each client has mounted (EXEC-TICK-4): the set a server-role kernel runs mount-scoped ticks for. A
//! client's entries leave when it unmounts them, when the host releases the client, and when its session is freed
//! (EXEC-DIST-9). The client role's own enter/leave ledger (`EmbeddedKernel::enter`) emits the `$UNMOUNT` legs this one
//! consumes.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(super) struct MountLedger {
    by_client: BTreeMap<String, BTreeSet<(String, String)>>,
}

impl MountLedger {
    pub(super) fn mount(
        &mut self,
        client: &str,
        orbital: &str,
        traits: impl IntoIterator<Item = String>,
    ) {
        let held = self.by_client.entry(client.to_string()).or_default();
        held.extend(traits.into_iter().map(|t| (orbital.to_string(), t)));
        if held.is_empty() {
            self.by_client.remove(client);
        }
    }

    pub(super) fn unmount(
        &mut self,
        client: &str,
        orbital: &str,
        traits: impl IntoIterator<Item = String>,
    ) {
        let Some(held) = self.by_client.get_mut(client) else {
            return;
        };
        for t in traits {
            held.remove(&(orbital.to_string(), t));
        }
        if held.is_empty() {
            self.by_client.remove(client);
        }
    }

    pub(super) fn release(&mut self, client: &str) {
        self.by_client.remove(client);
    }

    pub(super) fn holds(&self, trait_name: &str) -> bool {
        self.by_client
            .values()
            .any(|held| held.iter().any(|(_, t)| t == trait_name))
    }
}

impl RuntimeKernel {
    /// `client` has mounted `traits` of `orbital`: their mount-scoped ticks run while any client holds them.
    pub fn mount_client<I, S>(&self, client: &str, orbital: &str, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.mount_ledger
            .write()
            .mount(client, orbital, traits.into_iter().map(Into::into));
    }

    /// `client` has unmounted `traits` of `orbital`.
    pub fn unmount_client<I, S>(&self, client: &str, orbital: &str, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.mount_ledger
            .write()
            .unmount(client, orbital, traits.into_iter().map(Into::into));
    }

    /// `client` is gone: everything it had mounted is released.
    pub fn release_client(&self, client: &str) {
        self.mount_ledger.write().release(client);
    }

    /// Whether some client has `trait_name` mounted.
    pub fn trait_mounted(&self, trait_name: &str) -> bool {
        self.mount_ledger.read().holds(trait_name)
    }

    /// Whether `trait_name` is listed on, or embedded by a trait listed on, a protocol endpoint page (`boot`, `http`,
    /// `stdio`): the kernel mounts it when the program starts, with no viewer, for the program's life (EXEC-ENDPOINT-3).
    pub fn endpoint_mounted(&self, trait_name: &str) -> bool {
        use orbital_core_base::protocols::{protocol_after_preflight, ProtocolSide};
        let mut found = false;
        self.each_inline_page(|page| {
            if found || !protocol_after_preflight(page.protocol_name()).is_some_and(|spec| spec.side != ProtocolSide::Client) {
                return;
            }
            let listed: Vec<String> = page.traits.iter().flatten().filter_map(|t| t.trait_name()).map(str::to_string).collect();
            found = listed.iter().any(|t| t == trait_name) || self.trait_refs_from(&listed).iter().any(|t| t == trait_name);
        });
        found
    }

    /// Whether `tick` of `trait_name` fires on this kernel's clock (EXEC-TICK-6): a `[background]` tick always; any other
    /// while a client holds its trait or while the trait is endpoint-mounted ([`Self::endpoint_mounted`]).
    pub fn tick_runs(&self, trait_name: &str, tick: &str) -> bool {
        self.tick_of(trait_name, tick).is_some_and(|t| t.background)
            || self.trait_mounted(trait_name)
            || self.endpoint_mounted(trait_name)
    }
}
