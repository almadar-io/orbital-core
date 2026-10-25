//! Shell Trait and Configuration
//!
//! Defines the `Shell` trait implemented by each target platform shell.
//! Kept in `orbital-core` so shell crates can implement it without depending on
//! the compiler, breaking circular dependencies when the compiler registers those
//! shells.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Shell error
#[derive(Debug, Error)]
pub enum ShellError {
    #[error("Shell not found: {0}")]
    NotFound(String),

    #[error("Invalid shell structure at {path}: {message}")]
    InvalidStructure { path: PathBuf, message: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Template error: {0}")]
    Template(String),
}

/// Result type for shell operations
pub type ShellResult<T> = Result<T, ShellError>;

/// Trait for shell implementations.
/// Each target platform implements this trait in its own crate.
pub trait Shell: Send + Sync {
    /// Shell identifier (e.g., "typescript", "android", "rust")
    fn name(&self) -> &'static str;

    /// Human-readable description
    fn description(&self) -> &'static str;

    /// File extensions this shell produces
    fn extensions(&self) -> &[&'static str];

    /// Path to the shell template directory
    fn template_path(&self) -> &Path;

    /// Initialize a new project from this shell
    fn init_project(&self, target_dir: &Path, project_name: &str) -> ShellResult<()>;

    /// Copy shell template to output directory
    fn copy_template(&self, output_dir: &Path) -> ShellResult<()>;

    /// Get the generated code output directory within the shell
    fn generated_dir(&self) -> &str;

    /// Validate that the output directory has a valid shell structure
    fn validate_structure(&self, output_dir: &Path) -> ShellResult<()>;

    /// Run build command for this shell (e.g., "npm run build", "cargo build")
    fn build_command(&self) -> &str;

    /// Run dev command for this shell
    fn dev_command(&self) -> &str;

    /// Run test command for this shell
    fn test_command(&self) -> &str;

    /// Clean generated files
    fn clean_generated(&self, output_dir: &Path) -> ShellResult<()> {
        let gen_dir = output_dir.join(self.generated_dir());
        if gen_dir.exists() {
            std::fs::remove_dir_all(&gen_dir)?;
        }
        std::fs::create_dir_all(&gen_dir)?;
        Ok(())
    }
}

/// Shell configuration
#[derive(Debug, Clone)]
pub struct ShellConfig {
    /// Shell name
    pub name: String,
    /// Path to shell template
    pub template_path: PathBuf,
    /// Output directory for generated code
    pub output_dir: PathBuf,
    /// Project name
    pub project_name: String,
}

/// Base shell implementation that can be used by specific shells
pub struct BaseShell {
    name: &'static str,
    description: &'static str,
    extensions: &'static [&'static str],
    template_path: PathBuf,
    generated_dir: &'static str,
    build_cmd: &'static str,
    dev_cmd: &'static str,
    test_cmd: &'static str,
}

impl BaseShell {
    /// Create a new base shell
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &'static str,
        description: &'static str,
        extensions: &'static [&'static str],
        template_path: PathBuf,
        generated_dir: &'static str,
        build_cmd: &'static str,
        dev_cmd: &'static str,
        test_cmd: &'static str,
    ) -> Self {
        Self {
            name,
            description,
            extensions,
            template_path,
            generated_dir,
            build_cmd,
            dev_cmd,
            test_cmd,
        }
    }
}

impl Shell for BaseShell {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn extensions(&self) -> &[&'static str] {
        self.extensions
    }

    fn template_path(&self) -> &Path {
        &self.template_path
    }

    fn generated_dir(&self) -> &str {
        self.generated_dir
    }

    fn build_command(&self) -> &str {
        self.build_cmd
    }

    fn dev_command(&self) -> &str {
        self.dev_cmd
    }

    fn test_command(&self) -> &str {
        self.test_cmd
    }

    fn init_project(&self, target_dir: &Path, project_name: &str) -> ShellResult<()> {
        // Copy template to target with placeholder replacement
        copy_dir_with_replacements(&self.template_path, target_dir, project_name)?;

        // Create generated directory
        let gen_dir = target_dir.join(self.generated_dir);
        std::fs::create_dir_all(&gen_dir)?;

        Ok(())
    }

    fn copy_template(&self, output_dir: &Path) -> ShellResult<()> {
        if !self.template_path.exists() {
            return Err(ShellError::NotFound(
                self.template_path.display().to_string(),
            ));
        }

        copy_dir_recursive(&self.template_path, output_dir)?;
        Ok(())
    }

    fn validate_structure(&self, output_dir: &Path) -> ShellResult<()> {
        if !output_dir.exists() {
            return Err(ShellError::InvalidStructure {
                path: output_dir.to_path_buf(),
                message: "Directory does not exist".to_string(),
            });
        }

        let gen_dir = output_dir.join(self.generated_dir);
        if !gen_dir.exists() {
            return Err(ShellError::InvalidStructure {
                path: gen_dir,
                message: "Generated directory does not exist".to_string(),
            });
        }

        Ok(())
    }
}

/// Entry names (file OR dir) present in a shell repo — now a git submodule —
/// that must NOT be copied into a generated scaffold: build/runtime cruft, plus
/// repo-only dev entries (CI, git hooks, editor, turbo cache) and the submodule
/// `.git` gitlink FILE. Mirrors what the shell's npm `files` allowlist excluded
/// back when the scaffold was baked from the published package instead of
/// checked out as a submodule. `.git` here is critical: a submodule's `.git` is
/// a file, so it must be excluded as an entry, not only as a directory.
fn is_excluded_scaffold_entry(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            "node_modules"
                | "generated"
                | ".git"
                | ".gitmodules"
                | ".github"
                | ".githooks"
                | ".turbo"
                | "dist"
                | "coverage"
                | ".vscode"
                | ".idea"
                | ".npmignore"
        )
    )
}

/// Copy directory recursively
fn copy_dir_recursive(src: &Path, dst: &Path) -> ShellResult<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if is_excluded_scaffold_entry(&entry.file_name()) {
            continue;
        }
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            std::fs::copy(&path, &dest_path)?;
        }
    }

    Ok(())
}

/// Copy directory recursively with placeholder replacement
fn copy_dir_with_replacements(src: &Path, dst: &Path, project_name: &str) -> ShellResult<()> {
    if !src.exists() {
        return Err(ShellError::NotFound(src.display().to_string()));
    }

    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }

    // Build replacement map
    let replacements = build_replacements(project_name);

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if is_excluded_scaffold_entry(&entry.file_name()) {
            continue;
        }
        if path.is_dir() {
            copy_dir_with_replacements(&path, &dest_path, project_name)?;
        } else {
            // Check if this is a text file that needs placeholder replacement
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let should_process = matches!(
                ext,
                // TypeScript/JavaScript
                "ts" | "tsx"
                    | "js"
                    | "jsx"
                    // Python
                    | "py"
                    | "pyi"
                    | "toml"
                    // Common
                    | "json"
                    | "md"
                    | "html"
                    | "css"
                    | "yaml"
                    | "yml"
                    | "env"
                    | "txt"
                    | "sh"
                    // Rust
                    | "rs"
                    // Swift
                    | "swift"
                    // Kotlin
                    | "kt"
                    | "kts"
            ) || path.file_name().is_some_and(|n| {
                let name = n.to_string_lossy();
                name.starts_with('.') && !name.contains("lock")
            });

            if should_process {
                // Read, replace, and write
                let content = std::fs::read_to_string(&path)?;
                let processed = replace_placeholders(&content, &replacements);
                std::fs::write(&dest_path, processed)?;
            } else {
                // Binary file - just copy
                std::fs::copy(&path, &dest_path)?;
            }
        }
    }

    Ok(())
}

/// Build replacement map from project name
fn build_replacements(project_name: &str) -> std::collections::HashMap<&'static str, String> {
    let mut map = std::collections::HashMap::new();

    // App naming
    map.insert("{{APP_NAME}}", project_name.to_string());
    map.insert(
        "{{APP_TITLE}}",
        crate::str_utils::to_title_case(project_name),
    );

    // Firebase/hosting (use kebab-case for URLs)
    let kebab_name = crate::str_utils::to_kebab_case_simple(project_name);
    map.insert("{{SITE_NAME}}", kebab_name.clone());
    map.insert("{{BACKEND_NAME}}", format!("{}-backend", kebab_name));
    map.insert("{{FIREBASE_PROJECT_ID}}", format!("{}-project", kebab_name));

    // Ports
    map.insert("{{SERVER_PORT}}", "3030".to_string());
    map.insert("{{CLIENT_PORT}}", "5173".to_string());

    map
}

/// Replace all placeholders in content
fn replace_placeholders(
    content: &str,
    replacements: &std::collections::HashMap<&str, String>,
) -> String {
    let mut result = content.to_string();
    for (placeholder, value) in replacements {
        result = result.replace(placeholder, value);
    }
    result
}
