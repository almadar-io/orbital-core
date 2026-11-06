//! The `gpu/` family at a native host: the asks go to the [`GpuAnswerer`] the embedding host installed (orbital-canvas
//! on a host with a GPU); a host that installed none answers `unsupported`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::completion::Completions;
use super::OsHost;
use crate::resumable::CallSite;
use orbital_core_base::gpu::GpuAnswerer;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::value::Value;
use std::sync::Arc;

fn unsupported(op: OsOp) -> OsError {
    OsError::new(
        OsErrorKind::Unsupported,
        format!("{}: this host has no gpu", op.name()),
    )
}

fn answerer(op: OsOp, host: &OsHost<'_>) -> Result<Arc<dyn GpuAnswerer>, OsError> {
    host.handles.gpu_answerer().ok_or_else(|| unsupported(op))
}

pub(super) fn answer(op: OsOp, args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    answerer(op, host)?.answer(op, args)
}

/// A deferring `gpu/` op under a completion route runs on a thread of its own, so a device request, a queue wait or a
/// map-read holds that thread and not the lane.
pub(super) fn later(
    op: OsOp,
    args: &[Value],
    key: &str,
    site: &CallSite,
    host: &OsHost<'_>,
) -> Result<(), OsError> {
    let gpu = answerer(op, host)?;
    let args = args.to_vec();
    Completions::run_later(&host.handles.completions, key, site, move || {
        gpu.answer(op, &args)
    })
    .map_err(|error| super::net::later_error(op, error, key))
}
