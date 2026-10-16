//! Drawable / draw-host capability sets, derived from the installed
//! `@almadar/core` pattern registry (`language_data`): a pattern's `drawHost`
//! flag (it declares a `DrawableNode[]` slot) and `drawable` flag (it is
//! grounded in core `ScenePos`). Type-derived flags, not a name list.
//!
//! Lives in orbital-core because BOTH sibling crates need it: the orbital-compiler
//! render-ui validator (accept `children` on a draw-host; gate each child's
//! drawable-ness) and the orbital-client render_probe (exempt a draw-host's painted
//! children from renderer-coverage). Their only shared upstream is this crate.

use std::collections::HashSet;
use std::sync::LazyLock;

fn flagged(flag: &str) -> HashSet<String> {
    let registry: serde_json::Value =
        serde_json::from_str(&crate::language_data::language_data_after_preflight().patterns)
            .unwrap_or_else(|e| panic!("installed patterns registry is not JSON: {e}"));
    registry["patterns"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, entry)| entry[flag].as_bool() == Some(true))
        .map(|(name, _)| name.clone())
        .collect()
}

/// Canvas DRAW-HOST patterns — those declaring a `DrawableNode[]` slot. A draw-host
/// paints its authored `children` as neutral drawable descriptors (routed to the
/// `drawables` prop at runtime), so `children` is accepted on it and each child must
/// be `drawable`.
pub static PATTERN_DRAW_HOST: LazyLock<HashSet<String>> = LazyLock::new(|| flagged("drawHost"));

/// DRAWABLE patterns — neutral scene descriptors grounded in core `ScenePos`
/// (draw-sprite / draw-shape / draw-text / draw-*-layer). Only these may compose
/// under a draw-host's `children`.
pub static PATTERN_DRAWABLE: LazyLock<HashSet<String>> = LazyLock::new(|| flagged("drawable"));

/// Check if a pattern is a canvas draw-host (accepts painted `children`).
pub fn is_draw_host(pattern_type: &str) -> bool {
    PATTERN_DRAW_HOST.contains(pattern_type)
}

/// Check if a pattern is a neutral drawable (may compose under a draw-host).
pub fn is_drawable(pattern_type: &str) -> bool {
    PATTERN_DRAWABLE.contains(pattern_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_follow_the_installed_registry_flags() {
        assert!(is_draw_host("canvas-2d") && is_drawable("canvas-2d"));
        assert!(is_draw_host("learning-canvas") && !is_drawable("learning-canvas"));
        assert!(is_drawable("draw-sprite") && !is_draw_host("draw-sprite"));
    }

    #[test]
    fn control_an_ordinary_pattern_is_neither() {
        assert!(!is_draw_host("button") && !is_drawable("button"));
        assert!(!is_draw_host("zz-not-a-pattern"));
    }
}
