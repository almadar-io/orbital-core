//! System information operators: CPU, memory, uptime

use orbital_core_base::value::ValueMap;
#[cfg(target_os = "linux")]
use orbital_core_base::error::EvalError;
use orbital_core_base::error::OrbitalError;
use crate::evaluator::context::BindingContext;
use crate::evaluator::operators::{EvalResult, Operator};
use orbital_core_base::value::Value;
use orbital_core_base::schema::sexpr::SExpression;

// ============================================================================
// CPU Usage Operator
// ============================================================================

/// `["oal/cpu-usage"]` - Returns current CPU usage as percentage (0-100)
///
/// # Returns
/// - Number: CPU usage percentage
///
/// # Example
/// ```json
/// ["set", "@entity.cpuUsage", ["oal/cpu-usage"]]
/// ```
pub struct OalCpuUsageOp;

impl Operator for OalCpuUsageOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let usage = get_cpu_usage()?;
        Ok(Value::Number(usage))
    }
}

fn get_cpu_usage() -> Result<f64, OrbitalError> {
    // Read /proc/stat for CPU usage on Linux
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        use std::thread;
        use std::time::Duration;

        // Read CPU stats twice with a small delay to calculate usage
        let stat1 = fs::read_to_string("/proc/stat").map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "oal/cpu-usage: Failed to read /proc/stat: {}",
                e
            )))
        })?;

        thread::sleep(Duration::from_millis(100));

        let stat2 = fs::read_to_string("/proc/stat").map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "oal/cpu-usage: Failed to read /proc/stat: {}",
                e
            )))
        })?;

        let (idle1, total1) = parse_cpu_stat(&stat1)?;
        let (idle2, total2) = parse_cpu_stat(&stat2)?;

        let idle_delta = idle2 - idle1;
        let total_delta = total2 - total1;

        if total_delta == 0 {
            return Ok(0.0);
        }

        let usage = 100.0 * (1.0 - (idle_delta as f64 / total_delta as f64));
        Ok((usage * 10.0).round() / 10.0) // Round to 1 decimal place
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Return mock data for non-Linux systems
        Ok(25.0)
    }
}

#[cfg(target_os = "linux")]
fn parse_cpu_stat(stat: &str) -> Result<(u64, u64), OrbitalError> {
    let line = stat.lines().next().ok_or_else(|| {
        OrbitalError::Eval(EvalError::InvalidArgument(
            "oal/cpu-usage: Empty /proc/stat".to_string(),
        ))
    })?;

    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 5 || parts[0] != "cpu" {
        return Err(OrbitalError::Eval(EvalError::InvalidArgument(
            "oal/cpu-usage: Invalid /proc/stat format".to_string(),
        )));
    }

    // cpu user nice system idle iowait irq softirq steal guest guest_nice
    let user: u64 = parts[1].parse().unwrap_or(0);
    let nice: u64 = parts[2].parse().unwrap_or(0);
    let system: u64 = parts[3].parse().unwrap_or(0);
    let idle: u64 = parts[4].parse().unwrap_or(0);
    let iowait: u64 = parts.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
    let irq: u64 = parts.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);
    let softirq: u64 = parts.get(7).and_then(|s| s.parse().ok()).unwrap_or(0);
    let steal: u64 = parts.get(8).and_then(|s| s.parse().ok()).unwrap_or(0);

    let total = user + nice + system + idle + iowait + irq + softirq + steal;
    let idle_total = idle + iowait;

    Ok((idle_total, total))
}

// ============================================================================
// Memory Info Operator
// ============================================================================

/// `["oal/memory-info"]` - Returns memory usage information
///
/// # Returns
/// Object with:
/// - `total`: Total memory in bytes
/// - `available`: Available memory in bytes
/// - `used`: Used memory in bytes
/// - `usedPercent`: Usage percentage (0-100)
///
/// # Example
/// ```json
/// ["set", "@entity.memory", ["oal/memory-info"]]
/// ```
pub struct OalMemoryInfoOp;

impl Operator for OalMemoryInfoOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let info = get_memory_info()?;
        Ok(info)
    }
}

fn get_memory_info() -> Result<Value, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;

        let meminfo = fs::read_to_string("/proc/meminfo").map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "oal/memory-info: Failed to read /proc/meminfo: {}",
                e
            )))
        })?;

        let mut total_kb: u64 = 0;
        let mut available_kb: u64 = 0;
        let mut free_kb: u64 = 0;
        let mut buffers_kb: u64 = 0;
        let mut cached_kb: u64 = 0;

        for line in meminfo.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let value: u64 = parts[1].parse().unwrap_or(0);
                match parts[0] {
                    "MemTotal:" => total_kb = value,
                    "MemAvailable:" => available_kb = value,
                    "MemFree:" => free_kb = value,
                    "Buffers:" => buffers_kb = value,
                    "Cached:" => cached_kb = value,
                    _ => {}
                }
            }
        }

        // If MemAvailable not present (older kernels), estimate it
        if available_kb == 0 {
            available_kb = free_kb + buffers_kb + cached_kb;
        }

        let total = total_kb * 1024;
        let available = available_kb * 1024;
        let used = total.saturating_sub(available);
        let used_percent = if total > 0 {
            ((used as f64 / total as f64) * 1000.0).round() / 10.0
        } else {
            0.0
        };

        let mut result = ValueMap::new();
        result.insert("total".to_string(), Value::Number(total as f64));
        result.insert("available".to_string(), Value::Number(available as f64));
        result.insert("used".to_string(), Value::Number(used as f64));
        result.insert("usedPercent".to_string(), Value::Number(used_percent));

        Ok(Value::Object(result))
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Return mock data for non-Linux systems
        let mut result = ValueMap::new();
        result.insert("total".to_string(), Value::Number(16_000_000_000.0));
        result.insert("available".to_string(), Value::Number(8_000_000_000.0));
        result.insert("used".to_string(), Value::Number(8_000_000_000.0));
        result.insert("usedPercent".to_string(), Value::Number(50.0));
        Ok(Value::Object(result))
    }
}

// ============================================================================
// Uptime Operator
// ============================================================================

/// `["oal/uptime"]` - Returns system uptime in seconds
///
/// # Returns
/// - Number: Uptime in seconds
///
/// # Example
/// ```json
/// ["set", "@entity.uptime", ["oal/uptime"]]
/// ```
pub struct OalUptimeOp;

impl Operator for OalUptimeOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let uptime = get_uptime()?;
        Ok(Value::Number(uptime))
    }
}

fn get_uptime() -> Result<f64, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;

        let uptime_str = fs::read_to_string("/proc/uptime").map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "oal/uptime: Failed to read /proc/uptime: {}",
                e
            )))
        })?;

        let parts: Vec<&str> = uptime_str.split_whitespace().collect();
        if parts.is_empty() {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "oal/uptime: Empty /proc/uptime".to_string(),
            )));
        }

        let seconds: f64 = parts[0].parse().map_err(|_| {
            OrbitalError::Eval(EvalError::InvalidArgument(
                "oal/uptime: Invalid uptime value".to_string(),
            ))
        })?;

        Ok(seconds)
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Return mock data for non-Linux systems
        Ok(86400.0) // 1 day
    }
}

// ============================================================================
// Load Average Operator
// ============================================================================

/// `["oal/load-avg"]` - Returns system load averages
///
/// # Returns
/// Object with:
/// - `one`: 1-minute load average
/// - `five`: 5-minute load average
/// - `fifteen`: 15-minute load average
///
/// # Example
/// ```json
/// ["set", "@entity.loadAvg", ["oal/load-avg"]]
/// ```
pub struct OalLoadAvgOp;

impl Operator for OalLoadAvgOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let load = get_load_avg()?;
        Ok(load)
    }
}

fn get_load_avg() -> Result<Value, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;

        let loadavg = fs::read_to_string("/proc/loadavg").map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "oal/load-avg: Failed to read /proc/loadavg: {}",
                e
            )))
        })?;

        let parts: Vec<&str> = loadavg.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "oal/load-avg: Invalid /proc/loadavg format".to_string(),
            )));
        }

        let one: f64 = parts[0].parse().unwrap_or(0.0);
        let five: f64 = parts[1].parse().unwrap_or(0.0);
        let fifteen: f64 = parts[2].parse().unwrap_or(0.0);

        let mut result = ValueMap::new();
        result.insert("one".to_string(), Value::Number(one));
        result.insert("five".to_string(), Value::Number(five));
        result.insert("fifteen".to_string(), Value::Number(fifteen));

        Ok(Value::Object(result))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut result = ValueMap::new();
        result.insert("one".to_string(), Value::Number(0.5));
        result.insert("five".to_string(), Value::Number(0.6));
        result.insert("fifteen".to_string(), Value::Number(0.7));
        Ok(Value::Object(result))
    }
}

// ============================================================================
// Hostname Operator
// ============================================================================

/// `["oal/hostname"]` - Returns the system hostname
///
/// # Returns
/// - String: System hostname
///
/// # Example
/// ```json
/// ["set", "@entity.hostname", ["oal/hostname"]]
/// ```
pub struct OalHostnameOp;

impl Operator for OalHostnameOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let hostname = get_hostname()?;
        Ok(Value::String(hostname))
    }
}

fn get_hostname() -> Result<String, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;

        let hostname = fs::read_to_string("/etc/hostname")
            .or_else(|_| fs::read_to_string("/proc/sys/kernel/hostname"))
            .map_err(|e| {
                OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "oal/hostname: Failed to read hostname: {}",
                    e
                )))
            })?;

        Ok(hostname.trim().to_string())
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok("unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_usage_returns_number() {
        let op = OalCpuUsageOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        if let Ok(Value::Number(n)) = result {
            assert!((0.0..=100.0).contains(&n));
        } else {
            panic!("Expected number value");
        }
    }

    #[test]
    fn test_memory_info_returns_object() {
        let op = OalMemoryInfoOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        if let Ok(Value::Object(obj)) = result {
            assert!(obj.contains_key("total"));
            assert!(obj.contains_key("available"));
            assert!(obj.contains_key("used"));
            assert!(obj.contains_key("usedPercent"));
        } else {
            panic!("Expected object value");
        }
    }

    #[test]
    fn test_uptime_returns_number() {
        let op = OalUptimeOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        if let Ok(Value::Number(n)) = result {
            assert!(n >= 0.0);
        } else {
            panic!("Expected number value");
        }
    }

    #[test]
    fn test_load_avg_returns_object() {
        let op = OalLoadAvgOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        if let Ok(Value::Object(obj)) = result {
            assert!(obj.contains_key("one"));
            assert!(obj.contains_key("five"));
            assert!(obj.contains_key("fifteen"));
        } else {
            panic!("Expected object value");
        }
    }

    #[test]
    fn test_hostname_returns_string() {
        let op = OalHostnameOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        assert!(matches!(result, Ok(Value::String(_))));
    }
}
