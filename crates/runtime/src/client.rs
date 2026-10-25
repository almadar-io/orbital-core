//! The renderer-neutral client: event transport (in-process kernel or HTTP),
//! the client-role kernel and the client circuit (slots, entities, trait
//! content) every renderer draws from.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

pub mod bridge;
pub mod calendar;
pub mod client_core;
pub mod client_kernel;
pub mod helpers;
pub mod http;
pub mod recurse;
pub mod render_probe;
pub mod runtime;
pub mod tick;

pub use client_kernel::ClientKernel;
#[cfg(feature = "client-http")]
pub use http::SyncServerBridge;
pub use http::{BridgeError, ServerBridgeBuilder};
