//! EXEC-CONC-7's host for a guest session: when the opener's host offers one, `program/open` runs the guest under a
//! host of its own, the same host the opener runs under, over a fresh kernel. Its self `program/dispatch`, its keyed
//! asks and its route mounts (EXEC-SEED-7) are then answered as they are for any served program, under the grants the
//! opener passed. Without one, a guest runs in the opener's kernel with no host ports.

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::resumable::DispatchOutcomes;
use super::RuntimeKernel;
use crate::effects::response::MountSeed;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use std::sync::Arc;

/// How a guest reaches the world outside it: `Live` with the opener's credentials and its declared store drivers,
/// `Mock` with no credentials and every row in the guest's own store (`program/open { integrations }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GuestIntegrations {
    #[default]
    Live,
    Mock,
}

/// Opens a host for each guest program.
pub trait GuestHosting: Send + Sync {
    /// A host whose credential source is the opener's under `Live` and holds none under `Mock`.
    fn open(&self, integrations: GuestIntegrations) -> Arc<dyn GuestHost>;
}

/// One guest's host over its own kernel.
pub trait GuestHost: Send + Sync {
    fn kernel(&self) -> Arc<RuntimeKernel>;
    fn register(&self, schema: OrbitalSchema) -> Result<(), String>;
    /// The route mount of `route` (`$MOUNT` first, its deferred work settled), then `seeds`.
    fn mount(&self, route: Option<&str>, seeds: &[MountSeed], payload: Value) -> Result<DispatchOutcomes, String>;
    fn dispatch(
        &self,
        trait_name: &str,
        event: &str,
        payload: Value,
    ) -> Result<DispatchOutcomes, String>;
    /// One tick pass of the guest on its host's clock: every due tick of a trait the guest's page holds, `[client]` ones
    /// included since a guest has no browser (EXEC-ENDPOINT-3a), and every due `[background]` tick; what they moved.
    fn tick_pass(&self) -> Result<DispatchOutcomes, String>;
}

impl RuntimeKernel {
    /// Host the guests this kernel opens with `hosting` from now on.
    pub fn set_guest_hosting(&self, hosting: Arc<dyn GuestHosting>) {
        *self.guest_hosting.write() = Some(hosting);
    }

    pub(super) fn guest_hosting(&self) -> Option<Arc<dyn GuestHosting>> {
        self.guest_hosting.read().clone()
    }
}
