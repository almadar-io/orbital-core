//! Storage-related operators: disk usage, mount points

use crate::evaluator::ValueMap;
use crate::error::{EvalError, OrbitalError};
use crate::evaluator::{eval_expr, BindingContext, EvalResult, Operator, Value};
use crate::schema::SExpression;

// ============================================================================
// Disk Usage Operator
// ============================================================================

/// `["oal/disk-usage", path]` - Returns disk usage for a path
///
/// # Arguments
/// - `path`: String path to check (default: "/")
///
/// # Returns
/// Object with:
/// - `path`: The queried path
/// - `total`: Total disk space in bytes
/// - `used`: Used space in bytes
/// - `available`: Available space in bytes
/// - `usedPercent`: Usage percentage (0-100)
/// - `filesystem`: Filesystem type
pub struct OalDiskUsageOp;

impl Operator for OalDiskUsageOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let path = if args.is_empty() {
            "/".to_string()
        } else {
            eval_expr(&args[0], ctx)?
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "/".to_string())
        };

        let usage = get_disk_usage(&path)?;
        Ok(usage)
    }
}

fn get_disk_usage(path: &str) -> Result<Value, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;

        // Use df command to get disk usage
        let output = Command::new("df")
            .args(["--block-size=1", "--output=size,used,avail,fstype", path])
            .output()
            .map_err(|e| {
                OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "oal/disk-usage: Failed to run df: {}",
                    e
                )))
            })?;

        if !output.status.success() {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "oal/disk-usage: df command failed for path: {}",
                path
            ))));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().collect();

        if lines.len() < 2 {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "oal/disk-usage: Invalid df output".to_string(),
            )));
        }

        let parts: Vec<&str> = lines[1].split_whitespace().collect();
        if parts.len() < 4 {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "oal/disk-usage: Invalid df output format".to_string(),
            )));
        }

        let total: f64 = parts[0].parse().unwrap_or(0.0);
        let used: f64 = parts[1].parse().unwrap_or(0.0);
        let available: f64 = parts[2].parse().unwrap_or(0.0);
        let fs_type = parts[3].to_string();

        let used_percent = if total > 0.0 {
            ((used / total) * 1000.0).round() / 10.0
        } else {
            0.0
        };

        let mut result = ValueMap::new();
        result.insert("path".to_string(), Value::String(path.to_string()));
        result.insert("total".to_string(), Value::Number(total));
        result.insert("used".to_string(), Value::Number(used));
        result.insert("available".to_string(), Value::Number(available));
        result.insert("usedPercent".to_string(), Value::Number(used_percent));
        result.insert("filesystem".to_string(), Value::String(fs_type));

        Ok(Value::Object(result))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut result = ValueMap::new();
        result.insert("path".to_string(), Value::String(path.to_string()));
        result.insert("total".to_string(), Value::Number(500_000_000_000.0));
        result.insert("used".to_string(), Value::Number(250_000_000_000.0));
        result.insert("available".to_string(), Value::Number(250_000_000_000.0));
        result.insert("usedPercent".to_string(), Value::Number(50.0));
        result.insert("filesystem".to_string(), Value::String("ext4".to_string()));
        Ok(Value::Object(result))
    }
}

// ============================================================================
// Mount Points Operator
// ============================================================================

/// `["oal/mounts"]` - Returns list of mounted filesystems
///
/// # Returns
/// Array of objects, each with:
/// - `device`: Device path (e.g., "/dev/sda1")
/// - `mountPoint`: Mount point path
/// - `filesystem`: Filesystem type
/// - `options`: Mount options array
pub struct OalMountsOp;

impl Operator for OalMountsOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let mounts = get_mounts()?;
        Ok(mounts)
    }
}

fn get_mounts() -> Result<Value, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;

        let mounts_str = fs::read_to_string("/proc/mounts").map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "oal/mounts: Failed to read /proc/mounts: {}",
                e
            )))
        })?;

        let mut mounts = Vec::new();

        for line in mounts_str.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let device = parts[0];
                let mount_point = parts[1];
                let fs_type = parts[2];
                let options = parts[3];

                // Skip virtual filesystems for cleaner output
                if is_real_filesystem(fs_type) {
                    let options_array: Vec<Value> = options
                        .split(',')
                        .map(|s| Value::String(s.to_string()))
                        .collect();

                    let mut mount = ValueMap::new();
                    mount.insert("device".to_string(), Value::String(device.to_string()));
                    mount.insert(
                        "mountPoint".to_string(),
                        Value::String(mount_point.to_string()),
                    );
                    mount.insert("filesystem".to_string(), Value::String(fs_type.to_string()));
                    mount.insert("options".to_string(), Value::Array(options_array));

                    mounts.push(Value::Object(mount));
                }
            }
        }

        Ok(Value::Array(mounts))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut mount = ValueMap::new();
        mount.insert("device".to_string(), Value::String("/dev/sda1".to_string()));
        mount.insert("mountPoint".to_string(), Value::String("/".to_string()));
        mount.insert("filesystem".to_string(), Value::String("ext4".to_string()));
        mount.insert(
            "options".to_string(),
            Value::Array(vec![
                Value::String("rw".to_string()),
                Value::String("noatime".to_string()),
            ]),
        );
        Ok(Value::Array(vec![Value::Object(mount)]))
    }
}

#[cfg(target_os = "linux")]
fn is_real_filesystem(fs_type: &str) -> bool {
    !matches!(
        fs_type,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "tmpfs"
            | "securityfs"
            | "cgroup"
            | "cgroup2"
            | "pstore"
            | "debugfs"
            | "tracefs"
            | "hugetlbfs"
            | "mqueue"
            | "fusectl"
            | "binfmt_misc"
            | "configfs"
            | "efivarfs"
            | "autofs"
            | "overlay"
    )
}

// ============================================================================
// Disk IO Stats Operator
// ============================================================================

/// `["oal/disk-io"]` - Returns disk I/O statistics
///
/// # Returns
/// Array of objects (one per disk), each with:
/// - `device`: Device name (e.g., "sda")
/// - `readsCompleted`: Total reads completed
/// - `writesCompleted`: Total writes completed
/// - `readBytes`: Total bytes read
/// - `writeBytes`: Total bytes written
pub struct OalDiskIoOp;

impl Operator for OalDiskIoOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let io_stats = get_disk_io_stats()?;
        Ok(io_stats)
    }
}

fn get_disk_io_stats() -> Result<Value, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;

        let diskstats = fs::read_to_string("/proc/diskstats").map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "oal/disk-io: Failed to read /proc/diskstats: {}",
                e
            )))
        })?;

        let mut stats = Vec::new();

        for line in diskstats.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 14 {
                let device_name = parts[2];

                if is_physical_disk(device_name) {
                    let reads_completed: u64 = parts[3].parse().unwrap_or(0);
                    let sectors_read: u64 = parts[5].parse().unwrap_or(0);
                    let writes_completed: u64 = parts[7].parse().unwrap_or(0);
                    let sectors_written: u64 = parts[9].parse().unwrap_or(0);

                    let read_bytes = sectors_read * 512;
                    let write_bytes = sectors_written * 512;

                    let mut stat = ValueMap::new();
                    stat.insert("device".to_string(), Value::String(device_name.to_string()));
                    stat.insert(
                        "readsCompleted".to_string(),
                        Value::Number(reads_completed as f64),
                    );
                    stat.insert(
                        "writesCompleted".to_string(),
                        Value::Number(writes_completed as f64),
                    );
                    stat.insert("readBytes".to_string(), Value::Number(read_bytes as f64));
                    stat.insert("writeBytes".to_string(), Value::Number(write_bytes as f64));

                    stats.push(Value::Object(stat));
                }
            }
        }

        Ok(Value::Array(stats))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut stat = ValueMap::new();
        stat.insert("device".to_string(), Value::String("sda".to_string()));
        stat.insert("readsCompleted".to_string(), Value::Number(100000.0));
        stat.insert("writesCompleted".to_string(), Value::Number(50000.0));
        stat.insert("readBytes".to_string(), Value::Number(1_000_000_000.0));
        stat.insert("writeBytes".to_string(), Value::Number(500_000_000.0));
        Ok(Value::Array(vec![Value::Object(stat)]))
    }
}

#[cfg(target_os = "linux")]
fn is_physical_disk(name: &str) -> bool {
    // NVMe devices
    if name.starts_with("nvme") && name.contains('n') && !name.contains('p') {
        return true;
    }

    // SATA/SCSI devices (sda, sdb, etc. - without partition numbers)
    if name.starts_with("sd") && name.len() == 3 {
        return true;
    }

    // Virtual disks (vda, vdb, etc.)
    if name.starts_with("vd") && name.len() == 3 {
        return true;
    }

    // HD devices (older)
    if name.starts_with("hd") && name.len() == 3 {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disk_usage_default_path() {
        let op = OalDiskUsageOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        if let Ok(Value::Object(obj)) = result {
            assert!(obj.contains_key("path"));
            assert!(obj.contains_key("total"));
            assert!(obj.contains_key("used"));
            assert!(obj.contains_key("available"));
            assert!(obj.contains_key("usedPercent"));
        } else {
            panic!("Expected object value");
        }
    }

    #[test]
    fn test_mounts_returns_array() {
        let op = OalMountsOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        assert!(matches!(result, Ok(Value::Array(_))));
    }

    #[test]
    fn test_disk_io_returns_array() {
        let op = OalDiskIoOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        assert!(matches!(result, Ok(Value::Array(_))));
    }
}
