//! Shell Trait and Configuration
//!
//! Defines the `Shell` trait implemented by each target platform shell.
//! Kept in `orbital-core` so shell crates can implement it without depending on
//! the compiler, breaking circular dependencies when the compiler registers those
//! shells.

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
