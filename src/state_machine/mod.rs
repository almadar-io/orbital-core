//! State Machine Core
//!
//! This module provides the core state machine functionality for Orbital:
//! - `StateMachineCore`: Processes events through state machine transitions
//! - `StateMachineManager`: Manages multiple trait instances across entities
//! - `TraitState`: Current state of a trait instance
//! - `TransitionResult`: Result of processing an event
//!
//! # Example
//!
//! ```
//! use orbital_core::state_machine::{StateMachineManager, EventPayload};
//! use orbital_core::evaluator::Value;
//! use std::collections::HashMap;
//!
//! // Create a manager and register traits
//! let mut manager = StateMachineManager::new();
//! // manager.register_trait(trait_def);
//!
//! // Process events
//! // let result = manager.process_event("MyTrait", None, &payload)?;
//! ```

mod core;
mod manager;
mod types;

pub use types::{
    ArmGuard, EventPayload, GuardResult, TraitInstanceId, TraitState, TransitionFailureReason,
    TransitionResult,
};

pub use core::StateMachineCore;
pub use manager::{ManagerStats, StateMachineManager};
