//! Power: battery, AC and thermal readings.

use super::*;

pub(super) const POWER_SUPPLY: &str = "/sys/class/power_supply";

pub(super) fn battery_status(op: OsOp) -> Result<Value, OsError> {
    let names = dir_names(op, POWER_SUPPLY)?;
    let Some(name) = names.iter().find(|n| n.starts_with("BAT")) else {
        return Ok(object([
            ("present", Value::Bool(false)),
            ("percentage", num(0.0)),
            ("status", text("unknown")),
            ("timeToEmpty", Value::Null),
            ("timeToFull", Value::Null),
        ]));
    };
    let base = format!("{POWER_SUPPLY}/{name}");
    let percentage = read_f64(&format!("{base}/capacity"))
        .ok_or_else(|| unsupported(op, format!("{base}/capacity is not readable")))?;
    let raw_status = read_trimmed(&format!("{base}/status"))
        .unwrap_or_default()
        .to_lowercase();
    let status = match raw_status.as_str() {
        "charging" => "charging",
        "discharging" => "discharging",
        "full" => "full",
        "not charging" => "not_charging",
        _ => "unknown",
    };
    let rate =
        read_f64(&format!("{base}/power_now")).or_else(|| read_f64(&format!("{base}/current_now")));
    let (now, full) = if read_f64(&format!("{base}/power_now")).is_some() {
        (
            read_f64(&format!("{base}/energy_now")),
            read_f64(&format!("{base}/energy_full")),
        )
    } else {
        (
            read_f64(&format!("{base}/charge_now")),
            read_f64(&format!("{base}/charge_full")),
        )
    };
    let hours = |amount: Option<f64>| match (amount, rate) {
        (Some(a), Some(r)) if r > 0.0 => Value::Number((a / r * 3600.0).round()),
        _ => Value::Null,
    };
    let to_empty = if status == "discharging" {
        hours(now)
    } else {
        Value::Null
    };
    let to_full = match (status, full, now) {
        ("charging", Some(f), Some(n)) => hours(Some((f - n).max(0.0))),
        _ => Value::Null,
    };
    Ok(object([
        ("present", Value::Bool(true)),
        ("percentage", num(percentage)),
        ("status", text(status)),
        ("timeToEmpty", to_empty),
        ("timeToFull", to_full),
    ]))
}

pub(super) fn ac_power(op: OsOp) -> Result<Value, OsError> {
    let mut mains = 0;
    let mut online = false;
    for name in dir_names(op, POWER_SUPPLY)? {
        let base = format!("{POWER_SUPPLY}/{name}");
        if read_trimmed(&format!("{base}/type")).as_deref() == Some("Mains") {
            mains += 1;
            online |= read_trimmed(&format!("{base}/online")).as_deref() == Some("1");
        }
    }
    if mains == 0 {
        return Err(unsupported(op, "the host reports no mains power supply"));
    }
    Ok(object([("onAc", Value::Bool(online))]))
}

pub(super) fn thermal_zones(op: OsOp) -> Result<Value, OsError> {
    const THERMAL: &str = "/sys/class/thermal";
    let mut zones = Vec::new();
    for name in dir_names(op, THERMAL)? {
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let base = format!("{THERMAL}/{name}");
        let Some(milli) = read_f64(&format!("{base}/temp")) else {
            continue;
        };
        zones.push(object([
            ("name", text(name)),
            ("temperature", num(milli / 1000.0)),
            (
                "type",
                text(read_trimmed(&format!("{base}/type")).unwrap_or_default()),
            ),
        ]));
    }
    Ok(object([("zones", Value::array(zones))]))
}
