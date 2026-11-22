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
    /// A host whose credential source is the opener's under `Live` and holds none under `Mock`. `cwd`, when given, is the
    /// directory the guest's `cwd` anchor names instead of the opener's (`program/open { cwd }`). Each `env` entry is what
    /// this guest reads for that variable, `None` being unset (`program/open { env }`). With `program`, the guest's
    /// `program/*` and `behavior/*` operators are answered by the opener's program port (`program/open { program }`).
    fn open(&self, integrations: GuestIntegrations, cwd: Option<&str>, env: &[(String, Option<String>)], program: bool) -> Arc<dyn GuestHost>;

    /// The resolved program at `path` and the lazy programs it carries, by the file each lazy page names: how
    /// `program/open { path }` reads its program on the host's worker instead of taking it as a value (EXEC-CONC-7).
    fn read_program(&self, path: &str) -> Result<(OrbitalSchema, std::collections::HashMap<String, OrbitalSchema>), crate::effects::executor::SubstrateFailure> {
        Err(crate::agent_ports::guest_failure(
            orbital_core_base::os::OsErrorKind::Unsupported,
            format!("program/open: this host cannot read the program at {path}"),
        ))
    }

    /// One `uses lazy` program of the installed `behavior`, resolved, by the file its lazy page names: how a guest opened
    /// from a program value naming `lazySource: { behavior }` loads a lazy page when it first needs it (EXEC-LOAD-2).
    fn lazy_program(&self, behavior: &str, file: &str) -> Result<OrbitalSchema, crate::effects::executor::SubstrateFailure> {
        let _ = file;
        Err(crate::agent_ports::guest_failure(
            orbital_core_base::os::OsErrorKind::Unsupported,
            format!("program/open: this host resolves no installed behavior, so the lazy pages of `{behavior}` cannot load"),
        ))
    }
}

/// One guest's host over its own kernel.
pub trait GuestHost: Send + Sync {
    fn kernel(&self) -> Arc<RuntimeKernel>;
    /// The value of the environment variable `name` on the host that runs this guest, if it is set.
    fn environment_of(&self, name: &str) -> Option<String>;
    /// The value of the credential `name` as this guest's host resolves it, if it holds one.
    fn credential_of(&self, name: &str) -> Option<String>;
    /// The `denied` answers the host delivered for this guest's deferred work: message and count.
    fn deferred_denials(&self) -> Vec<(String, u64)>;
    fn register(&self, schema: OrbitalSchema) -> Result<(), String>;
    /// The route mount of `route` (`$MOUNT` first, its deferred work settled), then `seeds`.
    fn mount(&self, route: Option<&str>, seeds: &[MountSeed], payload: Value) -> Result<DispatchOutcomes, String>;
    fn dispatch(
        &self,
        trait_name: &str,
        event: &str,
        payload: Value,
    ) -> Result<DispatchOutcomes, String>;
    /// A split guest's server leg: `payload` with the request `options` a viewer kernel posts, answered as this host
    /// answers a served program's events request (the `EventResponse` wire for `wire: true`).
    fn request(&self, payload: &Value, options: &orbital_core_base::value::ValueMap) -> Result<Value, crate::effects::executor::SubstrateFailure>;
    /// One inbound delivery `{ inbound, body, headers }` to this guest, answered as this host answers its own
    /// `/api/hooks/<name>`: the handler's `{ status, body }` (EXEC-INBOUND-1).
    fn inbound(&self, ask: &orbital_core_base::value::ValueMap) -> Result<Value, crate::effects::executor::SubstrateFailure> {
        let _ = ask;
        Err(crate::agent_ports::guest_failure(
            orbital_core_base::os::OsErrorKind::Unsupported,
            "program/dispatch: this guest's host delivers no inbound hook",
        ))
    }
    /// One step of `trait_name` on `row` and no cascade (`event` with `payload`, or the tick `tick`), run by this host
    /// with its I/O answered as this host answers a dispatch's, under the guest's grants: what `kernel/step` hands a
    /// hosted guest's step to (EXEC-METAL-7). A host that runs no single step refuses.
    ///
    /// With `leg` (a leg `begin_leg` opened), the step runs in that leg's session and commits: its outcome joins the
    /// leg's, for `finish_leg` to answer.
    fn step(&self, trait_name: &str, row: Option<&str>, event: &str, payload: Value, tick: Option<&str>, leg: Option<&str>) -> Result<DispatchOutcomes, String> {
        let _ = (trait_name, row, event, payload, tick, leg);
        Err("this guest's host runs no single step".to_string())
    }
    /// A split guest's server leg up to its dispatch, for a `.lolo` kernel to run the dispatch (`program/dispatch
    /// { lolo: "begin" }`): answered `{ answered }` when the request is answered before any dispatch, else
    /// `{ leg, run, states }`, the leg left open in its session.
    fn begin_leg(&self, payload: &Value, options: &orbital_core_base::value::ValueMap) -> Result<Value, crate::effects::executor::SubstrateFailure> {
        let _ = (payload, options);
        Err(crate::agent_ports::guest_failure(orbital_core_base::os::OsErrorKind::Unsupported, "program/dispatch: this guest's host opens no .lolo leg"))
    }
    /// A plain guest dispatch of `event` to each of `targets` (`program/dispatch ADDRESS payload { guest, lolo:
    /// "begin", plain: true }`) opened as a leg for a `.lolo` kernel to run: `{ leg, run, states }`, the run being
    /// `{ kind: "discover", traits, event, payload, entityId }`, each target its own seed in the guest's program session.
    fn begin_plain(&self, targets: &[String], event: &str, payload: Value) -> Result<Value, String> {
        let _ = (targets, event, payload);
        Err("this guest's host opens no .lolo leg".to_string())
    }
    /// A plain leg's outcomes, its first target and its event, for the guest's answer; the leg closes.
    fn finish_plain(&self, leg: &str) -> Result<(DispatchOutcomes, String, String), String> {
        let _ = leg;
        Err("this guest's host opens no .lolo leg".to_string())
    }
    /// Run `run` in the open leg's session, origin and locale.
    fn in_leg(&self, leg: &str, run: Box<dyn FnOnce() -> Result<Value, String> + '_>) -> Result<Value, String> {
        let _ = (leg, run);
        Err("this guest's host opens no .lolo leg".to_string())
    }
    /// Run `step` inside the open leg as the held completion `run` that [`Self::settle_leg`] handed it: in that
    /// completion's session and asker, in the leg's lineage.
    fn in_leg_run(&self, leg: &str, run: &str, step: Box<dyn FnOnce() -> Result<Value, String> + '_>) -> Result<Value, String> {
        let _ = (leg, run, step);
        Err("this guest's host opens no .lolo leg".to_string())
    }
    /// The open leg's `$MOUNT` work settled (EXEC-SEED-7, `program/dispatch { lolo: "settle", leg }`): wait until every
    /// ask the leg's steps deferred has completed and is held, then hand those completions to the leg as
    /// `{ runs: [{ run, trait, event, payload, entityId }] }` for its kernel to step with [`Self::in_leg_run`]; `runs`
    /// is empty once nothing the leg asked is still out.
    fn settle_leg(&self, leg: &str) -> Result<Value, String> {
        let _ = leg;
        Err("this guest's host opens no .lolo leg".to_string())
    }
    /// The open leg's answer from the outcomes its steps made, as the host answers a leg it ran itself
    /// (`program/dispatch { lolo: "finish", leg }`); the leg closes.
    fn finish_leg(&self, leg: &str) -> Result<Value, crate::effects::executor::SubstrateFailure> {
        let _ = leg;
        Err(crate::agent_ports::guest_failure(orbital_core_base::os::OsErrorKind::Unsupported, "program/dispatch: this guest's host opens no .lolo leg"))
    }
    /// Close an open leg its `.lolo` kernel failed (`program/dispatch { lolo: "abandon", leg, error }`), as the host
    /// ends a run of its own that failed: a tick is logged and due again, a held completion fails with `error`.
    fn abandon_leg(&self, leg: &str, error: &str) -> Result<(), String> {
        let _ = (leg, error);
        Err("this guest's host opens no .lolo leg".to_string())
    }
    /// One tick pass of the guest on its host's clock: every due tick of a trait the guest's page holds, `[client]` ones
    /// included since a guest has no browser (EXEC-ENDPOINT-3a), and every due `[background]` tick; what they moved.
    fn tick_pass(&self) -> Result<DispatchOutcomes, String>;
    /// The dispatches this guest's host ran on its own for a viewer since the last call (a completion that ran in a
    /// viewer's session), each `{ clientId, orbitalName, request, response }` as the host's live stream carries it: a
    /// split guest's viewer folds them as it folds a served app's host-run dispatches (EXEC-DIST-8).
    fn take_host_dispatches(&self) -> Vec<Value> {
        Vec::new()
    }
    /// Hold this guest's completions from now on for a `.lolo` kernel to run (`program/open { kernel: "lolo" }`)
    /// instead of running them; a host that cannot refuses.
    fn drive_by_lolo(&self) -> Result<(), String> {
        Err("this guest's host cannot hand its completions to a .lolo kernel".to_string())
    }
    /// Every held run no leg has begun yet, ticks first, each in the order it was held: a completion `{ run, trait,
    /// event, payload, entityId, failure, key }` or a tick `{ run, kind: "tick", trait, tick }`. `run` is the handle
    /// `program/dispatch { lolo: "begin", run }` opens a leg for; a run is offered on every pass until it is begun, so a
    /// pass its opener refused loses nothing.
    fn take_parked_runs(&self) -> Vec<Value> {
        Vec::new()
    }
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    /// This guest host's live stream for the viewer `options.clientId`, as its served program's `/api/events` carries
    /// it: what `program/live { clientId, guest }` follows (EXEC-CONC-7). A host with no stream of its own answers none.
    fn live_source(&self, options: &orbital_core_base::value::ValueMap) -> Option<crate::native_host::SubstrateSource> {
        let _ = options;
        None
    }
}

impl RuntimeKernel {
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    /// The live stream of the hosted guest `handle` for the viewer `options.clientId` (`program/live { clientId, guest }`):
    /// `None` when no guest has that handle or its host keeps no stream.
    pub fn guest_live_source(&self, handle: &str, options: &orbital_core_base::value::ValueMap) -> Option<crate::native_host::SubstrateSource> {
        let host = self.guests.read().get(handle).and_then(|session| session.host.clone())?;
        host.live_source(options)
    }

    /// Host the guests this kernel opens with `hosting` from now on.
    pub fn set_guest_hosting(&self, hosting: Arc<dyn GuestHosting>) {
        *self.guest_hosting.write() = Some(hosting);
    }

    pub(super) fn guest_hosting(&self) -> Option<Arc<dyn GuestHosting>> {
        self.guest_hosting.read().clone()
    }

    /// Run `settle` after each guest open and close: the open builds the program in transient forms and the close drops a
    /// whole kernel, and a host whose allocator keeps freed pages gives them back here.
    pub fn set_memory_settler(&self, settle: Arc<dyn Fn() + Send + Sync>) {
        *self.memory_settler.write() = Some(settle);
    }

    pub(super) fn settle_memory(&self) {
        let settle = self.memory_settler.read().clone();
        if let Some(settle) = settle {
            settle();
        }
    }
}
