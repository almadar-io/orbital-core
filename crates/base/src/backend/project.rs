//! Writing a generated project from a resolved program (the shell side of a compile).

use crate::generate::{Backend, GeneratedOutput, ShellMode};
use crate::ir::types::OirModule;
use crate::shell::{Shell, ShellError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// How [`write_project`] writes.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteOptions {
    /// Copy the shell template first.
    pub copy_template: bool,
    /// Remove existing generated files first.
    pub clean: bool,
    pub verbose: bool,
    /// Full, client or server.
    pub mode: ShellMode,
    /// Service id -> runtimes it runs on; `None` reads the installed services registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_runtimes: Option<HashMap<String, Vec<String>>>,
}

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("Shell error: {0}")]
    Shell(#[from] ShellError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Backend error: {0}")]
    Backend(String),
}

/// The manifest file name `generate_from_oir` reads/writes at `output_dir`,
/// listing every logical `GeneratedFile::path` written by its OWN write loop
/// (never `.env`/lowered-`.orb`/`schema.orb`/`resolved.orb`, written by other
/// call sites).
pub const ORB_MANIFEST_FILE: &str = ".orb-manifest.json";

/// A generated path prefixed with one of these is "workspace-relative" —
/// already rooted at a template-declared location outside the shell's own
/// `generated_dir` tree (a sibling package root, or a native test target's
/// own `sources:`/source-set root declared in `project.yml`/`build.gradle.kts`)
/// — and is written directly under `output_dir` instead of nested under
/// `generated_dir`. `packages/` is the original TS/mobile multi-package
/// case (`orbital-shell-typescript`/`orbital-shell-mobile`); `Sources/` and
/// `app/src/test/` are the iOS/Android native test-target cases (Program C
/// wave C4 — `Sources/OrbitalAppTests/PatternSnapshotTests.swift`,
/// `app/src/test/kotlin/generated/PatternSnapshotTests.kt`); `app/src/main/assets/`
/// is the Android *main* module's asset root (Program B wave B6 —
/// `app/src/main/assets/resolved_schema.json`, the in-app kernel's bundled
/// resolved schema, mirroring iOS's `Sources/OrbitalApp/Resources/`) — added
/// because neither shell's `generated_dir` (`Sources/OrbitalApp/Generated`,
/// `app/src/main/kotlin/generated`) covers a sibling resource/test source
/// root. No existing `GeneratedFile::path` in any shell started with any of
/// these before their own addition (each backend otherwise emits bare
/// relative names under its own `generated_dir`), so each is purely additive.
const WORKSPACE_RELATIVE_PREFIXES: &[&str] =
    &["packages/", "Sources/", "app/src/test/", "app/src/main/assets/"];

/// Write a generated project for `oir`: copy the shell template, clean, run the
/// backend, refuse codegen markers, write the files and prune what the previous
/// write produced and this one no longer does. Shell-side only: every shell
/// program runs it on the resolved program `orb` hands it.
pub fn write_project<S, B>(
    oir: &OirModule,
    output_dir: impl AsRef<Path>,
    shell: &S,
    backend: &B,
    options: &WriteOptions,
) -> Result<GeneratedOutput, WriteError>
where
    S: Shell,
    B: Backend,
{
    let output_dir = output_dir.as_ref();

    if options.verbose {
        eprintln!("Output: {}", output_dir.display());
        eprintln!("Shell: {}", shell.name());
        eprintln!("Backend: {}", backend.name());
    }

    // Step 2: Copy shell template if requested
    if options.copy_template {
        if options.verbose {
            eprintln!("Copying shell template...");
        }
        shell.init_project(output_dir, &oir.name)?;
    }

    // Step 3: Clean existing generated files if requested
    if options.clean {
        if options.verbose {
            eprintln!("Cleaning generated directory...");
        }
        shell.clean_generated(output_dir)?;
    }

    // Step 4: Generate code
    if options.verbose {
        eprintln!("Generating code (mode: {:?})...", options.mode);
    }
    ensure_backend_supports(oir, backend)?;
    ensure_services_hostable(oir, backend, options)?;
    let output = backend.generate_with_mode(oir, options.mode);

    // Cross-shell operator-completeness backstop: no emitted file may carry the
    // shared unsupported-operator marker (Python emits it in a `#` comment, TS
    // in a `/* */` comment). Fail loudly BEFORE writing, rather than shipping a
    // broken stub the static parity gate can't see — e.g. an operator with no
    // canonical-registry entry at all.
    let unsupported: Vec<String> = output
        .files
        .iter()
        .flat_map(|file| {
            file.content
                .lines()
                .filter(|line| {
                    line.contains(crate::generate::UNSUPPORTED_OPERATOR_MARKER)
                        || line.contains(crate::ir::literal::LITERAL_TYPE_MISMATCH_MARKER)
                })
                .map(move |line| format!("{} — {}", file.path, line.trim()))
        })
        .collect();
    if !unsupported.is_empty() {
        return Err(WriteError::Backend(format!(
            "backend emitted {} codegen marker(s) (unsupported operator / default literal \
             not renderable at its declared type):\n  {}",
            unsupported.len(),
            unsupported.join("\n  ")
        )));
    }

    // Step 5: Write files
    // The output first: `create_dir_all("<new>/.")` fails, since `.` hides `<new>` from the parent walk.
    std::fs::create_dir_all(output_dir)?;
    let generated_dir = output_dir.join(shell.generated_dir());
    std::fs::create_dir_all(&generated_dir)?;

    // §94 (`docs/Almadar_Compiler_Gaps.md`): a source-level trait/organism a
    // recompile no longer emits used to leave its LAST compile's file behind
    // forever — nothing in this loop ever deletes anything. Read the PREVIOUS
    // compile's manifest before writing (`None` on first compile / no
    // manifest / an unparsable one — never destructive on that path), then
    // prune whatever it lists that this compile did not reproduce. Scoped to
    // exactly what THIS loop writes: `generate_from_file`'s `schema.orb` /
    // `resolved.orb`, `orbital-cli`'s `.env` merge, and the lowered `.orb`
    // (`orbital-cli/src/commands/compile.rs`) are written by other call
    // sites entirely and never touched here.
    let previous_manifest = read_orb_manifest(output_dir);

    for file in &output.files {
        // Workspace-relative paths (e.g. packages/shared/...) are written
        // relative to the output root; all other generated paths live under
        // the shell's generated_dir.
        let file_path = resolve_manifest_path(output_dir, &generated_dir, &file.path);

        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&file_path, &file.content)?;

        if options.verbose {
            eprintln!("  Written: {}", file.path);
        }
    }

    let new_manifest: std::collections::BTreeSet<String> =
        output.files.iter().map(|f| f.path.clone()).collect();
    if let Some(previous) = previous_manifest {
        for stale in previous.difference(&new_manifest) {
            let stale_path = resolve_manifest_path(output_dir, &generated_dir, stale);
            if stale_path.exists() {
                std::fs::remove_file(&stale_path)?;
                if options.verbose {
                    eprintln!("  Pruned stale: {stale}");
                }
                if let Some(parent) = stale_path.parent() {
                    prune_empty_dirs(parent, output_dir);
                }
            }
        }
    }
    write_orb_manifest(output_dir, &new_manifest)?;

    if options.verbose {
        eprintln!(
            "Generated {} files to {}",
            output.files.len(),
            generated_dir.display()
        );
    }

    Ok(output)
}

/// A program declaring browser-stored entities compiles only with a backend that
/// generates their client-side data effects; any other backend is refused
/// before it writes a file.
fn ensure_backend_supports<B: Backend>(oir: &OirModule, backend: &B) -> Result<(), WriteError> {
    let local: Vec<&str> = oir.entities.iter().filter(|e| e.browser).map(|e| e.name.as_str()).collect();
    if local.is_empty() || backend.supports_browser_stored_entities() {
        return Ok(());
    }
    Err(WriteError::Backend(format!(
        "ORB_SHELL_LOCAL_ENTITY_UNSUPPORTED: the {} shell does not generate browser-stored entities; {} declare `browser`. Compile with --shell typescript, or drop `browser`.",
        backend.name(),
        local.join(", ")
    )))
}

/// A browser-only service's calls run on the browser leg; a full build of a
/// shell that generates no browser leg has nowhere to run them, so it is
/// refused before it writes a file. A client or server build is one half of a
/// split deployment whose other half may host the call, so it is not judged here.
fn ensure_services_hostable<B: Backend>(oir: &OirModule, backend: &B, options: &WriteOptions) -> Result<(), WriteError> {
    use crate::runs_on::{is_browser_only, SERVICE_RUNTIME_UNSUPPORTED};
    if options.mode != ShellMode::Full || backend.supports_browser_stored_entities() {
        return Ok(());
    }
    let runtimes = options
        .service_runtimes
        .as_ref()
        .unwrap_or(&crate::language::registry::baked_service_registry().runtimes);
    let refused: Vec<String> = oir
        .invoked_services()
        .into_iter()
        .filter(|service| runtimes.get(service).is_some_and(|r| is_browser_only(r)))
        .collect();
    if refused.is_empty() {
        return Ok(());
    }
    Err(WriteError::Backend(format!(
        "{SERVICE_RUNTIME_UNSUPPORTED}: the {} shell generates no browser leg, and {} run(s) only in the browser. Compile with --shell typescript.",
        backend.name(),
        refused.join(", ")
    )))
}

/// A workspace-relative path (see `WORKSPACE_RELATIVE_PREFIXES`) is written
/// relative to the output root; every other generated path is written under
/// the shell's `generated_dir` — the ONE resolution rule the write loop and
/// the stale-prune pass both use, so a manifest entry always resolves to the
/// exact file it named.
fn resolve_manifest_path(output_dir: &Path, generated_dir: &Path, logical_path: &str) -> std::path::PathBuf {
    if WORKSPACE_RELATIVE_PREFIXES.iter().any(|prefix| logical_path.starts_with(prefix)) {
        output_dir.join(logical_path)
    } else {
        generated_dir.join(logical_path)
    }
}

/// The previous compile's manifest, if `output_dir` has one and it parses —
/// `None` on first compile, a missing file, or anything unparsable, so a
/// corrupt/foreign manifest is never destructive (no prune runs at all
/// rather than guessing at a malformed list).
fn read_orb_manifest(output_dir: &Path) -> Option<std::collections::BTreeSet<String>> {
    let content = std::fs::read_to_string(output_dir.join(ORB_MANIFEST_FILE)).ok()?;
    serde_json::from_str(&content).ok()
}

/// Persist this compile's manifest for the NEXT compile's prune pass.
fn write_orb_manifest(
    output_dir: &Path,
    manifest: &std::collections::BTreeSet<String>,
) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(output_dir.join(ORB_MANIFEST_FILE), json)
}

/// Remove `dir` and every now-empty ancestor up to (but never including)
/// `stop_at`, bottom-up — a stale file's directory (and that directory's own
/// now-empty parent, …) left behind by a dropped trait/organism. Stops the
/// instant a directory is missing, not a directory, non-empty, or `stop_at`
/// itself; never touches `stop_at` or anything above it.
fn prune_empty_dirs(dir: &Path, stop_at: &Path) {
    let mut current = dir;
    loop {
        if current == stop_at || !current.starts_with(stop_at) {
            return;
        }
        let Ok(mut entries) = std::fs::read_dir(current) else {
            return;
        };
        if entries.next().is_some() {
            return;
        }
        if std::fs::remove_dir(current).is_err() {
            return;
        }
        let Some(parent) = current.parent() else {
            return;
        };
        current = parent;
    }
}

// The program's `public` folder (copied as-is into a generated app).
/// The public folder on disk, or why it cannot be used: it must exist and
/// stay inside the program's own folder.
pub fn resolve_public_dir(source_path: &Path, public_dir: &str) -> Result<PathBuf, String> {
    let base = source_path.parent().unwrap_or(Path::new("."));
    let dir = base.join(public_dir);
    let (Ok(base), Ok(dir)) = (base.canonicalize(), dir.canonicalize()) else {
        return Err(format!(
            "public folder `{public_dir}` does not exist beside the program"
        ));
    };
    if !dir.is_dir() {
        return Err(format!("public `{public_dir}` is not a folder"));
    }
    if !dir.starts_with(&base) {
        return Err(format!(
            "public folder `{public_dir}` is outside the program's folder"
        ));
    }
    Ok(dir)
}

/// Copy `{source_dir}/locales/*.json` into `{output}/locales/` (a generated app reads `locales/{locale}.json`), with an empty catalog for
/// each built-in locale the project does not provide.
pub fn copy_locales(source_path: &Path, output: &Path) -> std::io::Result<()> {
    let destination = output.join("locales");
    std::fs::create_dir_all(&destination)?;
    if let Some(source_locales) = source_path.parent().map(|d| d.join("locales")).filter(|d| d.is_dir()) {
        for entry in std::fs::read_dir(source_locales)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                std::fs::copy(&path, destination.join(entry.file_name()))?;
            }
        }
    }
    for lang in ["en", "ar", "sl"] {
        let path = destination.join(format!("{lang}.json"));
        if !path.exists() {
            std::fs::write(path, "{}")?;
        }
    }
    Ok(())
}

/// Copies `from` into `to`, recursively, byte for byte.
pub fn copy_public(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_public(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OneFile;

    impl Backend for OneFile {
        fn generate(&self, _module: &OirModule) -> GeneratedOutput {
            GeneratedOutput { files: vec![crate::generate::GeneratedFile::new("src/app.txt", "hi")], warnings: vec![] }
        }
        fn name(&self) -> &'static str {
            "one-file"
        }
    }

    fn shell(generated_dir: &'static str) -> crate::shell::BaseShell {
        crate::shell::BaseShell::new("t", "t", &[], PathBuf::from("/nonexistent-template"), generated_dir, "", "", "")
    }

    #[test]
    fn a_project_at_the_root_is_written_into_a_fresh_directory_without_a_template() {
        let root = tempfile::TempDir::new().unwrap();
        let out = root.path().join("fresh");
        write_project(&OirModule::new("App", "1.0.0"), &out, &shell("."), &OneFile, &WriteOptions::default()).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("src/app.txt")).unwrap(), "hi");
    }

    #[test]
    fn control_a_nested_generated_dir_is_written_into_a_fresh_directory() {
        let root = tempfile::TempDir::new().unwrap();
        let out = root.path().join("fresh");
        write_project(&OirModule::new("App", "1.0.0"), &out, &shell("gen"), &OneFile, &WriteOptions::default()).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("gen/src/app.txt")).unwrap(), "hi");
    }

    fn site() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("public/img")).unwrap();
        std::fs::write(
            dir.path().join("public/install.sh"),
            "#!/bin/sh\necho orb\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("public/img/logo.svg"), "<svg/>").unwrap();
        std::fs::write(dir.path().join("site.lolo"), "").unwrap();
        dir
    }

    #[test]
    fn copies_the_folder_byte_for_byte() {
        let s = site();
        let from = resolve_public_dir(&s.path().join("site.lolo"), "./public").unwrap();
        let out = tempfile::TempDir::new().unwrap();
        copy_public(&from, out.path()).unwrap();
        assert_eq!(
            std::fs::read(out.path().join("install.sh")).unwrap(),
            b"#!/bin/sh\necho orb\n"
        );
        assert_eq!(
            std::fs::read(out.path().join("img/logo.svg")).unwrap(),
            b"<svg/>"
        );
    }

    #[test]
    fn a_missing_folder_is_rejected() {
        let s = site();
        assert!(resolve_public_dir(&s.path().join("site.lolo"), "./static")
            .unwrap_err()
            .contains("does not exist"));
    }

    #[test]
    fn edge_a_folder_outside_the_program_is_rejected() {
        let s = site();
        std::fs::create_dir_all(s.path().join("nested")).unwrap();
        std::fs::write(s.path().join("nested/site.lolo"), "").unwrap();
        assert!(
            resolve_public_dir(&s.path().join("nested/site.lolo"), "../public")
                .unwrap_err()
                .contains("outside")
        );
    }
}
