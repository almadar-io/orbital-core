//! The seam a host answers `gpu/*` asks through: the kernel unwraps handles to ids and mints the sealed values.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::os::{OsError, OsOp};
use crate::value::Value;

pub trait GpuAnswerer: Send + Sync {
    /// Answer one gpu/* ask. Handle arguments arrive as their plain ids (the kernel unwrapped them); bytes arrive as Value::Bytes.
    fn answer(&self, op: OsOp, args: &[Value]) -> Result<Value, OsError>;
    /// Drop every object this answerer holds (host teardown).
    fn close_all(&self);
}
