//! Power-related operators: battery status, thermal information

use crate::evaluator::ValueMap;
use crate::error::OrbitalError;
use crate::evaluator::{BindingContext, EvalResult, Operator, Value};
use crate::schema::SExpression;

// ============================================================================
// Battery Status Operator
// ============================================================================

/// `["oal/battery-status"]` - Returns battery information
///
/// # Returns
/// Object with:
/// - `present`: Boolean indicating if battery is present
/// - `percentage`: Charge percentage (0-100)
/// - `status`: String ("charging", "discharging", "full", "not_charging", "unknown")
/// - `timeToEmpty`: Estimated seconds until empty (if discharging)
/// - `timeToFull`: Estimated seconds until full (if charging)
pub struct OalBatteryStatusOp;

impl Operator for OalBatteryStatusOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let status = get_battery_status()?;
        Ok(status)
    }
}

fn get_battery_status() -> Result<Value, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        use std::path::Path;

        let power_supply_path = Path::new("/sys/class/power_supply");
        let mut battery_found = false;
        let mut result = ValueMap::new();

        if let Ok(entries) = fs::read_dir(power_supply_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if name_str.starts_with("BAT") {
                    battery_found = true;
                    let bat_path = entry.path();

                    let capacity = fs::read_to_string(bat_path.join("capacity"))
                        .ok()
                        .and_then(|s| s.trim().parse::<f64>().ok())
                        .unwrap_or(0.0);

                    let status = fs::read_to_string(bat_path.join("status"))
                        .ok()
                        .map(|s| s.trim().to_lowercase())
                        .unwrap_or_else(|| "unknown".to_string());

                    let normalized_status = match status.as_str() {
                        "charging" => "charging",
                        "discharging" => "discharging",
                        "full" => "full",
                        "not charging" => "not_charging",
                        _ => "unknown",
                    };

                    result.insert("present".to_string(), Value::Bool(true));
                    result.insert("percentage".to_string(), Value::Number(capacity));
                    result.insert(
                        "status".to_string(),
                        Value::String(normalized_status.to_string()),
                    );

                    break;
                }
            }
        }

        if !battery_found {
            result.insert("present".to_string(), Value::Bool(false));
            result.insert("percentage".to_string(), Value::Number(0.0));
            result.insert("status".to_string(), Value::String("unknown".to_string()));
        }

        Ok(Value::Object(result))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut result = ValueMap::new();
        result.insert("present".to_string(), Value::Bool(true));
        result.insert("percentage".to_string(), Value::Number(75.0));
        result.insert(
            "status".to_string(),
            Value::String("discharging".to_string()),
        );
        result.insert("timeToEmpty".to_string(), Value::Number(7200.0));
        Ok(Value::Object(result))
    }
}

// ============================================================================
// Thermal Zones Operator
// ============================================================================

/// `["oal/thermal-zones"]` - Returns thermal zone temperatures
///
/// # Returns
/// Array of objects, each with:
/// - `name`: Zone name
/// - `temperature`: Temperature in Celsius
/// - `type`: Zone type (e.g., "x86_pkg_temp", "acpitz")
pub struct OalThermalZonesOp;

impl Operator for OalThermalZonesOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let zones = get_thermal_zones()?;
        Ok(zones)
    }
}

fn get_thermal_zones() -> Result<Value, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        use std::path::Path;

        let thermal_path = Path::new("/sys/class/thermal");
        let mut zones = Vec::new();

        if let Ok(entries) = fs::read_dir(thermal_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if name_str.starts_with("thermal_zone") {
                    let zone_path = entry.path();

                    let temp_mc = fs::read_to_string(zone_path.join("temp"))
                        .ok()
                        .and_then(|s| s.trim().parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let temp_c = temp_mc / 1000.0;

                    let zone_type = fs::read_to_string(zone_path.join("type"))
                        .ok()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    let mut zone = ValueMap::new();
                    zone.insert("name".to_string(), Value::String(name_str.to_string()));
                    zone.insert("temperature".to_string(), Value::Number(temp_c));
                    zone.insert("type".to_string(), Value::String(zone_type));

                    zones.push(Value::Object(zone));
                }
            }
        }

        Ok(Value::Array(zones))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut zone = ValueMap::new();
        zone.insert(
            "name".to_string(),
            Value::String("thermal_zone0".to_string()),
        );
        zone.insert("temperature".to_string(), Value::Number(45.0));
        zone.insert(
            "type".to_string(),
            Value::String("x86_pkg_temp".to_string()),
        );
        Ok(Value::Array(vec![Value::Object(zone)]))
    }
}

// ============================================================================
// AC Power Status Operator
// ============================================================================

/// `["oal/ac-power"]` - Returns whether AC power is connected
///
/// # Returns
/// - Boolean: true if AC power is connected
pub struct OalAcPowerOp;

impl Operator for OalAcPowerOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let on_ac = is_on_ac_power()?;
        Ok(Value::Bool(on_ac))
    }
}

fn is_on_ac_power() -> Result<bool, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        use std::path::Path;

        let power_supply_path = Path::new("/sys/class/power_supply");

        if let Ok(entries) = fs::read_dir(power_supply_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if name_str.starts_with("AC")
                    || name_str.starts_with("ADP")
                    || name_str.contains("Mains")
                {
                    let ac_path = entry.path();
                    let online = fs::read_to_string(ac_path.join("online"))
                        .ok()
                        .and_then(|s| s.trim().parse::<u8>().ok())
                        .unwrap_or(0);

                    return Ok(online == 1);
                }
            }
        }

        Ok(true)
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_battery_status_returns_object() {
        let op = OalBatteryStatusOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        if let Ok(Value::Object(obj)) = result {
            assert!(obj.contains_key("present"));
            assert!(obj.contains_key("percentage"));
            assert!(obj.contains_key("status"));
        } else {
            panic!("Expected object value");
        }
    }

    #[test]
    fn test_thermal_zones_returns_array() {
        let op = OalThermalZonesOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        assert!(matches!(result, Ok(Value::Array(_))));
    }

    #[test]
    fn test_ac_power_returns_bool() {
        let op = OalAcPowerOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        assert!(matches!(result, Ok(Value::Bool(_))));
    }
}
