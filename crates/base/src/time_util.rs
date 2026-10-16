//! Wall-clock helper portable to `wasm32-unknown-unknown`.
//!
//! `std::time::SystemTime::now()` and `chrono::Utc::now()` panic (`unimplemented!`) on
//! bare `wasm32-unknown-unknown`, which is what the egui-via-wasm renderer compiles to.
//! Native keeps the existing `SystemTime`/`chrono` behavior; wasm reads the clock via
//! `js_sys::Date`. One convergence point so every operator/state-machine timestamp stays
//! in sync across both execution paths.

/// Milliseconds since the Unix epoch.
#[cfg(target_arch = "wasm32")]
pub fn now_millis() -> i64 {
    js_sys::Date::now() as i64
}

#[cfg(not(target_arch = "wasm32"))]
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Milliseconds since the Unix epoch, as `f64` (the shape the time operators want).
pub fn now_millis_f64() -> f64 {
    now_millis() as f64
}

/// Current time as an RFC3339 / ISO-8601 string (metadata stamps; the `@now`
/// binding is `now_millis_f64` — ms-epoch on every execution surface).
#[cfg(target_arch = "wasm32")]
pub fn now_rfc3339() -> String {
    js_sys::Date::new_0()
        .to_iso_string()
        .as_string()
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}
