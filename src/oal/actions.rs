//! Action operators: notifications, brightness, volume, speech

use crate::evaluator::ValueMap;
use crate::error::{EvalError, OrbitalError};
use crate::evaluator::{eval_expr, BindingContext, EvalResult, Operator, Value};
use crate::schema::SExpression;
#[cfg(target_os = "linux")]
use std::process::Command;

// ============================================================================
// Desktop Notification Operator
// ============================================================================

/// `["oal/notify", title, body, urgency?]` - Send a desktop notification
///
/// # Arguments
/// - `title`: Notification title
/// - `body`: Notification body text
/// - `urgency`: Optional urgency level ("low", "normal", "critical")
///
/// # Returns
/// - Boolean: true if notification was sent successfully
pub struct OalNotifyOp;

impl Operator for OalNotifyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "oal/notify".to_string(),
                expected: 2,
                actual: args.len(),
            }));
        }

        let title = eval_expr(&args[0], ctx)?
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Notification".to_string());

        let body = if args.len() > 1 {
            eval_expr(&args[1], ctx)?
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let urgency = if args.len() > 2 {
            eval_expr(&args[2], ctx)?
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "normal".to_string())
        } else {
            "normal".to_string()
        };

        let success = send_notification(&title, &body, &urgency)?;
        Ok(Value::Bool(success))
    }
}

fn send_notification(title: &str, body: &str, urgency: &str) -> Result<bool, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        let mut cmd = Command::new("notify-send");

        let urgency_flag = match urgency {
            "low" => "low",
            "critical" => "critical",
            _ => "normal",
        };

        cmd.arg("--urgency").arg(urgency_flag);
        cmd.arg(title);
        cmd.arg(body);

        match cmd.status() {
            Ok(status) => Ok(status.success()),
            Err(e) => {
                eprintln!("Warning: Failed to send notification: {}", e);
                Ok(false)
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("NOTIFICATION [{}]: {} - {}", urgency, title, body);
        Ok(true)
    }
}

// ============================================================================
// Text-to-Speech Operator
// ============================================================================

/// `["oal/speak", text]` - Speak text using text-to-speech
///
/// # Arguments
/// - `text`: Text to speak
///
/// # Returns
/// - Boolean: true if speech was initiated successfully
pub struct OalSpeakOp;

impl Operator for OalSpeakOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "oal/speak".to_string(),
                expected: 1,
                actual: args.len(),
            }));
        }

        let text = eval_expr(&args[0], ctx)?
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_default();

        let success = speak_text(&text)?;
        Ok(Value::Bool(success))
    }
}

fn speak_text(text: &str) -> Result<bool, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        let result = Command::new("espeak-ng")
            .arg(text)
            .status()
            .or_else(|_| Command::new("espeak").arg(text).status());

        match result {
            Ok(status) => Ok(status.success()),
            Err(e) => {
                eprintln!("Warning: Failed to speak text: {}", e);
                Ok(false)
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("SPEAK: {}", text);
        Ok(true)
    }
}

// ============================================================================
// Brightness Control Operators
// ============================================================================

/// `["oal/brightness-get"]` - Get current screen brightness
///
/// # Returns
/// - Number: Brightness percentage (0-100)
pub struct OalBrightnessGetOp;

impl Operator for OalBrightnessGetOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let brightness = get_brightness()?;
        Ok(Value::Number(brightness))
    }
}

fn get_brightness() -> Result<f64, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        use std::path::Path;

        let backlight_path = Path::new("/sys/class/backlight");

        if let Ok(entries) = fs::read_dir(backlight_path) {
            for entry in entries.flatten() {
                let bl_path = entry.path();

                let current = fs::read_to_string(bl_path.join("brightness"))
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok());

                let max = fs::read_to_string(bl_path.join("max_brightness"))
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok());

                if let (Some(curr), Some(max)) = (current, max) {
                    if max > 0.0 {
                        return Ok((curr / max * 100.0).round());
                    }
                }
            }
        }

        Ok(100.0)
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(75.0)
    }
}

/// `["oal/brightness-set", value]` - Set screen brightness
///
/// # Arguments
/// - `value`: Brightness percentage (0-100)
///
/// # Returns
/// - Boolean: true if brightness was set successfully
pub struct OalBrightnessSetOp;

impl Operator for OalBrightnessSetOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "oal/brightness-set".to_string(),
                expected: 1,
                actual: args.len(),
            }));
        }

        let value = eval_expr(&args[0], ctx)?.to_number().ok_or_else(|| {
            OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "number".to_string(),
                actual: "non-number".to_string(),
            })
        })?;

        let value = value.clamp(0.0, 100.0);
        let success = set_brightness(value)?;
        Ok(Value::Bool(success))
    }
}

fn set_brightness(percentage: f64) -> Result<bool, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        // Use brightnessctl if available
        if Command::new("brightnessctl")
            .arg("set")
            .arg(format!("{}%", percentage as u64))
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(true);
        }

        Ok(false)
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("SET BRIGHTNESS: {}%", percentage);
        Ok(true)
    }
}

// ============================================================================
// Volume Control Operators
// ============================================================================

/// `["oal/volume-get"]` - Get current audio volume
///
/// # Returns
/// Object with:
/// - `volume`: Volume percentage (0-100)
/// - `muted`: Boolean indicating if audio is muted
pub struct OalVolumeGetOp;

impl Operator for OalVolumeGetOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let volume = get_volume()?;
        Ok(volume)
    }
}

fn get_volume() -> Result<Value, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        let output = Command::new("pactl")
            .args(["get-sink-volume", "@DEFAULT_SINK@"])
            .output()
            .or_else(|_| Command::new("amixer").args(["get", "Master"]).output());

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let mut volume = 50.0;
                let mut muted = false;

                if let Some(pos) = stdout.find('%') {
                    let before = &stdout[..pos];
                    if let Some(num_start) = before.rfind(|c: char| !c.is_ascii_digit()) {
                        if let Ok(v) = before[num_start + 1..].trim().parse::<f64>() {
                            volume = v;
                        }
                    }
                }

                let mute_output = Command::new("pactl")
                    .args(["get-sink-mute", "@DEFAULT_SINK@"])
                    .output();

                if let Ok(mute_out) = mute_output {
                    let mute_str = String::from_utf8_lossy(&mute_out.stdout);
                    muted = mute_str.contains("yes");
                }

                let mut result = ValueMap::new();
                result.insert("volume".to_string(), Value::Number(volume));
                result.insert("muted".to_string(), Value::Bool(muted));
                Ok(Value::Object(result))
            }
            Err(_) => {
                let mut result = ValueMap::new();
                result.insert("volume".to_string(), Value::Number(50.0));
                result.insert("muted".to_string(), Value::Bool(false));
                Ok(Value::Object(result))
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut result = ValueMap::new();
        result.insert("volume".to_string(), Value::Number(75.0));
        result.insert("muted".to_string(), Value::Bool(false));
        Ok(Value::Object(result))
    }
}

/// `["oal/volume-set", value]` - Set audio volume
///
/// # Arguments
/// - `value`: Volume percentage (0-100)
///
/// # Returns
/// - Boolean: true if volume was set successfully
pub struct OalVolumeSetOp;

impl Operator for OalVolumeSetOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "oal/volume-set".to_string(),
                expected: 1,
                actual: args.len(),
            }));
        }

        let value = eval_expr(&args[0], ctx)?.to_number().ok_or_else(|| {
            OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "number".to_string(),
                actual: "non-number".to_string(),
            })
        })?;

        let value = value.clamp(0.0, 100.0);
        let success = set_volume(value)?;
        Ok(Value::Bool(success))
    }
}

fn set_volume(percentage: f64) -> Result<bool, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        let result = Command::new("pactl")
            .args([
                "set-sink-volume",
                "@DEFAULT_SINK@",
                &format!("{}%", percentage as u64),
            ])
            .status()
            .or_else(|_| {
                Command::new("amixer")
                    .args(["set", "Master", &format!("{}%", percentage as u64)])
                    .status()
            });

        match result {
            Ok(status) => Ok(status.success()),
            Err(e) => {
                eprintln!("Warning: Failed to set volume: {}", e);
                Ok(false)
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("SET VOLUME: {}%", percentage);
        Ok(true)
    }
}

/// `["oal/volume-mute", muted?]` - Toggle or set mute state
///
/// # Arguments
/// - `muted`: Optional boolean. If not provided, toggles current state.
///
/// # Returns
/// - Boolean: true if operation was successful
pub struct OalVolumeMuteOp;

impl Operator for OalVolumeMuteOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let mute_state = if args.is_empty() {
            None
        } else {
            Some(eval_expr(&args[0], ctx)?.as_bool().unwrap_or(true))
        };

        let success = set_mute(mute_state)?;
        Ok(Value::Bool(success))
    }
}

fn set_mute(mute_state: Option<bool>) -> Result<bool, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        let state_arg = match mute_state {
            Some(true) => "1",
            Some(false) => "0",
            None => "toggle",
        };

        let result = Command::new("pactl")
            .args(["set-sink-mute", "@DEFAULT_SINK@", state_arg])
            .status()
            .or_else(|_| {
                let amixer_state = match mute_state {
                    Some(true) => "mute",
                    Some(false) => "unmute",
                    None => "toggle",
                };
                Command::new("amixer")
                    .args(["set", "Master", amixer_state])
                    .status()
            });

        match result {
            Ok(status) => Ok(status.success()),
            Err(e) => {
                eprintln!("Warning: Failed to set mute: {}", e);
                Ok(false)
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("SET MUTE: {:?}", mute_state);
        Ok(true)
    }
}

// ============================================================================
// System Power Actions
// ============================================================================

/// `["oal/suspend"]` - Suspend the system
///
/// # Returns
/// - Boolean: true if suspend command was initiated
pub struct OalSuspendOp;

impl Operator for OalSuspendOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let success = suspend_system()?;
        Ok(Value::Bool(success))
    }
}

fn suspend_system() -> Result<bool, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        let result = Command::new("systemctl")
            .arg("suspend")
            .status()
            .or_else(|_| Command::new("pm-suspend").status());

        match result {
            Ok(status) => Ok(status.success()),
            Err(e) => {
                eprintln!("Warning: Failed to suspend: {}", e);
                Ok(false)
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("SUSPEND SYSTEM");
        Ok(true)
    }
}

/// `["oal/hibernate"]` - Hibernate the system
///
/// # Returns
/// - Boolean: true if hibernate command was initiated
pub struct OalHibernateOp;

impl Operator for OalHibernateOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let success = hibernate_system()?;
        Ok(Value::Bool(success))
    }
}

fn hibernate_system() -> Result<bool, OrbitalError> {
    #[cfg(target_os = "linux")]
    {
        let result = Command::new("systemctl")
            .arg("hibernate")
            .status()
            .or_else(|_| Command::new("pm-hibernate").status());

        match result {
            Ok(status) => Ok(status.success()),
            Err(e) => {
                eprintln!("Warning: Failed to hibernate: {}", e);
                Ok(false)
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("HIBERNATE SYSTEM");
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_brightness_get_returns_number() {
        let op = OalBrightnessGetOp;
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
    fn test_volume_get_returns_object() {
        let op = OalVolumeGetOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_ok());
        if let Ok(Value::Object(obj)) = result {
            assert!(obj.contains_key("volume"));
            assert!(obj.contains_key("muted"));
        } else {
            panic!("Expected object value");
        }
    }

    #[test]
    fn test_notify_requires_args() {
        let op = OalNotifyOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_speak_requires_args() {
        let op = OalSpeakOp;
        let ctx = BindingContext::new();
        let result = op.evaluate(&[], &ctx);
        assert!(result.is_err());
    }
}
