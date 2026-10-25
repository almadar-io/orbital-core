//! Code Generation Types
//!
//! Shared types used by compiler backends. Kept in `orbital-core` so shell crates
//! can implement `Backend` without depending on the compiler.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::ir::types::OirModule;

/// Marker embedded in generated source when a shell backend has no real
/// implementation for an operator. Shared across shells (Python wraps it in a
/// `#` comment, TypeScript in a `/* */` comment) so the compiler's pre-write
/// output scan can hard-fail on any of them regardless of target language.
pub const UNSUPPORTED_OPERATOR_MARKER: &str = "UNSUPPORTED operator:";

/// The shared unsupported-operator marker text for a specific operator, e.g.
/// `unsupported_operator_marker("example/unknown") == "UNSUPPORTED operator: example/unknown"`.
/// Each shell wraps this in its own comment syntax; the substring stays greppable.
pub fn unsupported_operator_marker(function: &str) -> String {
    format!("{UNSUPPORTED_OPERATOR_MARKER} {function}")
}

/// A generated file
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GeneratedFile {
    /// Relative path within the generated directory
    pub path: String,
    /// File content
    pub content: String,
}

impl GeneratedFile {
    pub fn new(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }
}

/// Output from code generation
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GeneratedOutput {
    /// Generated files
    pub files: Vec<GeneratedFile>,
    /// Warnings during generation
    pub warnings: Vec<String>,
}

/// Shell mode for mixed-language compilation
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellMode {
    /// Generate everything (default)
    #[default]
    Full,
    /// Client-only: pages, components, client effects
    Client,
    /// Server-only: routes, handlers, server effects
    Server,
}

/// Backend trait for code generation
///
/// Each shell (TypeScript, Android, etc.) implements this trait to generate
/// code from OIR.
pub trait Backend: Send + Sync {
    /// Generate code from an OIR module
    fn generate(&self, module: &OirModule) -> GeneratedOutput;

    /// Generate code with mode awareness
    fn generate_with_mode(&self, module: &OirModule, _mode: ShellMode) -> GeneratedOutput {
        // Default implementation ignores mode and generates everything
        self.generate(module)
    }

    /// Backend name (e.g., "typescript", "android")
    fn name(&self) -> &'static str;

    /// Whether this backend generates browser-side `fetch`/`persist` for
    /// `[persistent: x, browser]` entities. A backend that does not is refused a
    /// program declaring one (`ORB_SHELL_LOCAL_ENTITY_UNSUPPORTED`).
    fn supports_browser_stored_entities(&self) -> bool {
        false
    }
}

/// The indexable public routes of `page`'s translation group (its
/// `translationOf`, else its upstream `sourcePage`) across the app's locales,
/// in locale order, as `[{ locale, path }]` — its hreflang alternates. `None`
/// when `page` is not itself indexable and public, has no group, appears in fewer than two locales, or one locale holds it
/// twice (no single alternate per language).
pub fn locale_alternates(
    oir: &crate::ir::types::OirModule,
    page: &crate::ir::types::OirPage,
) -> Option<serde_json::Value> {
    use crate::schema::types::{PageAccess, PageIndexing};
    let indexable = |p: &crate::ir::types::OirPage| {
        p.access == Some(PageAccess::Public) && p.indexing != Some(PageIndexing::Noindex)
    };
    if !indexable(page) {
        return None;
    }
    let group = |p: &crate::ir::types::OirPage| -> Option<String> {
        p.translation_of.clone().or_else(|| p.source_page.clone())
    };
    let key = group(page)?;
    let mut by_locale: Vec<(&str, &str)> = Vec::new();
    for other in oir
        .pages
        .iter()
        .filter(|p| group(p).as_deref() == Some(key.as_str()) && indexable(p))
    {
        let locale = oir.locale_of_path(&other.path)?;
        if by_locale.iter().any(|(l, _)| *l == locale) {
            return None;
        }
        by_locale.push((locale, other.path.as_str()));
    }
    if by_locale.len() < 2 {
        return None;
    }
    by_locale.sort_by_key(|(l, _)| oir.locales.iter().position(|x| x == l));
    Some(
        by_locale
            .into_iter()
            .map(|(locale, path)| serde_json::json!({ "locale": locale, "path": path }))
            .collect(),
    )
}
