//! OAL (Orbital Abstraction Layer) - System-level std library extensions
//!
//! These operators provide access to system information and actions for
//! AMA OS (Ambient Machine Agent Operating System) schemas.
//!
//! # Operator Naming Convention
//! All OAL operators use the `oal/` prefix:
//! - `oal/cpu-usage` - Get current CPU usage percentage
//! - `oal/memory-info` - Get memory usage information
//! - `oal/uptime` - Get system uptime
//! - `oal/battery-status` - Get battery information
//! - `oal/disk-usage` - Get disk usage for a path
//! - `oal/notify` - Send desktop notification
//!
//! # Usage in S-Expressions
//! ```json
//! ["set", "@entity.cpuUsage", ["oal/cpu-usage"]]
//! ["set", "@entity.memory", ["oal/memory-info"]]
//! ["oal/notify", "Alert", "CPU usage high!", "warning"]
//! ```

pub mod actions;
pub mod network;
pub mod power;
pub mod storage;
pub mod system;

// Re-export all operators for easy registration
pub use actions::*;
pub use network::*;
pub use power::*;
pub use storage::*;
pub use system::*;
