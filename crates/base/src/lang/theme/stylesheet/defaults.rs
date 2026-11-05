#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::BTreeMap;

/// Default values for tokens not specified in the schema
pub(crate) fn get_default_tokens() -> BTreeMap<&'static str, &'static str> {
    let mut defaults = BTreeMap::new();

    // Shadows
    defaults.insert("shadow-none", "none");
    defaults.insert("shadow-sm", "0 1px 2px rgba(0, 0, 0, 0.05)");
    defaults.insert("shadow-main", "0 4px 6px rgba(0, 0, 0, 0.1)");
    defaults.insert("shadow-lg", "0 10px 15px rgba(0, 0, 0, 0.1)");
    defaults.insert("shadow-inner", "inset 0 2px 4px rgba(0, 0, 0, 0.05)");
    defaults.insert("shadow-hover", "0 8px 25px rgba(0, 0, 0, 0.15)");
    defaults.insert("shadow-active", "0 2px 4px rgba(0, 0, 0, 0.1)");

    // Border widths
    defaults.insert("border-width", "1px");
    defaults.insert("border-width-thin", "1px");
    defaults.insert("border-width-thick", "2px");

    // Typography
    defaults.insert(
        "font-family",
        "'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
    );
    defaults.insert("font-weight-normal", "400");
    defaults.insert("font-weight-medium", "500");
    defaults.insert("font-weight-bold", "600");
    defaults.insert("letter-spacing", "0");
    defaults.insert("line-height", "1.5");

    // Transitions
    defaults.insert("transition-fast", "100ms");
    defaults.insert("transition-normal", "200ms");
    defaults.insert("transition-slow", "300ms");
    defaults.insert("transition-timing", "ease-in-out");

    // Transforms
    defaults.insert("hover-scale", "1.02");
    defaults.insert("hover-translate-y", "-2px");
    defaults.insert("hover-translate-x", "0");
    defaults.insert("active-scale", "0.98");
    defaults.insert("active-translate-y", "1px");

    // Focus
    defaults.insert("focus-ring-width", "2px");
    defaults.insert("focus-ring-offset", "2px");

    // Components
    defaults.insert("icon-stroke-width", "2");
    defaults.insert("icon-color", "currentColor");
    defaults.insert("divider-style", "solid");

    defaults
}

/// Default colors for light mode
pub(crate) fn get_default_colors_light() -> BTreeMap<&'static str, &'static str> {
    let mut colors = BTreeMap::new();

    // Core semantic colors
    colors.insert("primary", "#0ea5e9");
    colors.insert("primary-hover", "#0284c7");
    colors.insert("primary-foreground", "#ffffff");

    colors.insert("secondary", "#f1f5f9");
    colors.insert("secondary-hover", "#e2e8f0");
    colors.insert("secondary-foreground", "#0f172a");

    colors.insert("accent", "#8b5cf6");
    colors.insert("accent-foreground", "#ffffff");

    // Backgrounds
    colors.insert("background", "#ffffff");
    colors.insert("foreground", "#0f172a");
    colors.insert("surface", "#f8fafc");

    // Muted/subtle
    colors.insert("muted", "#f1f5f9");
    colors.insert("muted-foreground", "#64748b");

    // Borders
    colors.insert("border", "#e2e8f0");

    // Input
    colors.insert("input", "#ffffff");

    // Status colors
    colors.insert("success", "#22c55e");
    colors.insert("success-foreground", "#ffffff");
    colors.insert("warning", "#f59e0b");
    colors.insert("warning-foreground", "#ffffff");
    colors.insert("error", "#ef4444");
    colors.insert("error-foreground", "#ffffff");
    colors.insert("info", "#3b82f6");
    colors.insert("info-foreground", "#ffffff");

    // Card/Panel
    colors.insert("card", "#ffffff");
    colors.insert("card-foreground", "#0f172a");

    // Ring (focus)
    colors.insert("ring", "#0ea5e9");

    colors
}

/// Default colors for dark mode
pub(crate) fn get_default_colors_dark() -> BTreeMap<&'static str, &'static str> {
    let mut colors = BTreeMap::new();

    // Core semantic colors
    colors.insert("primary", "#38bdf8");
    colors.insert("primary-hover", "#7dd3fc");
    colors.insert("primary-foreground", "#0c4a6e");

    colors.insert("secondary", "#334155");
    colors.insert("secondary-hover", "#475569");
    colors.insert("secondary-foreground", "#f8fafc");

    colors.insert("accent", "#a78bfa");
    colors.insert("accent-foreground", "#1e1b4b");

    // Backgrounds
    colors.insert("background", "#0f172a");
    colors.insert("foreground", "#f8fafc");
    colors.insert("surface", "#1e293b");

    // Muted/subtle
    colors.insert("muted", "#1e293b");
    colors.insert("muted-foreground", "#94a3b8");

    // Borders
    colors.insert("border", "#334155");

    // Input
    colors.insert("input", "#1e293b");

    // Status colors - brighter for dark mode
    colors.insert("success", "#4ade80");
    colors.insert("success-foreground", "#052e16");
    colors.insert("warning", "#fbbf24");
    colors.insert("warning-foreground", "#451a03");
    colors.insert("error", "#f87171");
    colors.insert("error-foreground", "#450a0a");
    colors.insert("info", "#60a5fa");
    colors.insert("info-foreground", "#1e3a8a");

    // Card/Panel
    colors.insert("card", "#1e293b");
    colors.insert("card-foreground", "#f8fafc");

    // Ring (focus)
    colors.insert("ring", "#38bdf8");

    colors
}

/// Default border radii
pub(crate) fn get_default_radii() -> BTreeMap<&'static str, &'static str> {
    let mut radii = BTreeMap::new();
    radii.insert("none", "0");
    radii.insert("sm", "4px");
    radii.insert("md", "8px");
    radii.insert("lg", "12px");
    radii.insert("xl", "16px");
    radii.insert("full", "9999px");
    radii
}
