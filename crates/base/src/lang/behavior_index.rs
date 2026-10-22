//! The structural half of a behavior package's index, derived from its tree and
//! `.lolo` headers on read: which behaviors a package ships, each one's topic,
//! level and exposure, the `@capabilities` it declares, and its description.
//! The program-side half (entity, events, traits) needs the lowered programs
//! and lives in `orbital-cli`'s `behaviors_index`.
//!
//! There is no index file: the old `behaviors-registry.json` carried exactly
//! these values, written by a TS extraction. The rules here are that
//! extraction's, ported 1:1.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::behavior_packages::{index_registry_source, BehaviorPackage, DiscoverError};

/// The directory names that mark a composition tier.
pub const TIER_DIRS: [&str; 4] = ["atoms", "molecules", "organisms", "templates"];

/// The values `@exposure` and an exposure default may take.
pub const EXPOSURES: [&str; 5] = ["app", "experience", "palette", "both", "internal"];

/// `.hand-authored.json`: the file naming a topic's blessed hand-authored boards.
const HAND_AUTHORED_FILE: &str = ".hand-authored.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Atom,
    Molecule,
    Organism,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Atom => "atom",
            Level::Molecule => "molecule",
            Level::Organism => "organism",
        }
    }

    fn tier(self) -> &'static str {
        match self {
            Level::Atom => "atoms",
            Level::Molecule => "molecules",
            Level::Organism => "organisms",
        }
    }
}

/// One behavior a package ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub name: String,
    pub path: PathBuf,
    /// The directories between the package's registry (or source) root and the file.
    pub dirs: Vec<String>,
}

impl IndexedFile {
    /// The first directory, or `ui/<domain>` under the `ui` substrate.
    pub fn topic(&self) -> String {
        match (self.dirs.first(), self.dirs.get(1)) {
            (Some(head), Some(next)) if head == "ui" => format!("ui/{next}"),
            (Some(head), _) => head.clone(),
            (None, _) => String::new(),
        }
    }

    /// The tier directory the file sits in directly decides its level; any other parent is an atom.
    pub fn level(&self) -> Level {
        match self.dirs.last().map(String::as_str) {
            Some("molecules") => Level::Molecule,
            Some("organisms") => Level::Organism,
            _ => Level::Atom,
        }
    }

    /// The tier the exposure defaults read: the parent directory when it is a tier name, else the level's.
    pub fn tier(&self) -> &str {
        match self.dirs.last() {
            Some(last) if TIER_DIRS.contains(&last.as_str()) => last,
            _ => self.level().tier(),
        }
    }

    /// The path under the root, up to the LAST tier directory (`ui/core` for `ui/core/atoms/x`);
    /// empty when the file sits under no tier directory.
    fn tiered_topic(&self) -> &[String] {
        match self
            .dirs
            .iter()
            .rposition(|d| TIER_DIRS.contains(&d.as_str()))
        {
            Some(i) if i >= 1 => &self.dirs[..i],
            _ => &[],
        }
    }
}

/// Every behavior `pkg` ships, in path order: the files of its registry (or source) tree that its
/// `files` allowlist covers.
pub fn indexed_files(pkg: &BehaviorPackage) -> Result<Vec<IndexedFile>, DiscoverError> {
    let mut files: Vec<(String, PathBuf)> = index_registry_source(&pkg.registry)?
        .into_iter()
        .filter(|(_, path)| pkg.ships(path))
        .collect();
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(files
        .into_iter()
        .map(|(name, path)| indexed_file_at(pkg, name, path))
        .collect())
}

/// The behavior `name` whose registry program is at `path`, placed under `pkg`'s registry root.
pub fn indexed_file_at(pkg: &BehaviorPackage, name: String, path: PathBuf) -> IndexedFile {
    let dirs = path
        .parent()
        .and_then(|p| p.strip_prefix(pkg.registry.root()).ok())
        .map(|rel| {
            rel.components()
                .filter_map(|c| match c {
                    Component::Normal(s) => s.to_str().map(str::to_string),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    IndexedFile { name, path, dirs }
}

/// The names of the behaviors `pkg` ships.
pub fn indexed_names(pkg: &BehaviorPackage) -> Result<HashSet<String>, DiscoverError> {
    Ok(indexed_files(pkg)?.into_iter().map(|f| f.name).collect())
}

// ---------------------------------------------------------------------------
// `.lolo` header
// ---------------------------------------------------------------------------

/// The `.lolo` header: every line before the first `orbital` block.
fn header_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().take_while(|line| {
        let t = line.trim_start();
        !(t.starts_with("orbital")
            && t["orbital".len()..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_')))
    })
}

/// The text after `;; @<tag>` on a header line, trimmed; `None` when the line is not that tag.
fn tag_rest<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let rest = line.trim_start().strip_prefix(";;")?.trim_start();
    let rest = rest.strip_prefix('@')?.strip_prefix(tag)?;
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}

/// A quoted value: the text between the quotes, which hold no quote themselves.
fn quoted(value: &str) -> Option<&str> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains('"')).then_some(inner)
}

/// `[a-z0-9]+(-[a-z0-9]+)*`.
fn is_kebab(v: &str) -> bool {
    !v.is_empty()
        && v.split('-').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// An `@exposure`-shaped value: lowercase letters, quoted or bare.
fn lower_word(value: &str) -> Option<&str> {
    let word = quoted(value).unwrap_or(value);
    (!word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase())).then_some(word)
}

/// The behavior-level tags a `.lolo` header declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderTags {
    /// `;; @exposure <value>`
    pub exposure: Option<String>,
    /// `;; @capabilities "phrase, phrase"`
    pub capabilities: Option<String>,
    /// `;; @pattern "<type>"`: the `@almadar/ui` pattern a generated `ui-*` behavior wraps 1:1.
    pub pattern: Option<String>,
}

/// Reads the `@exposure`, `@capabilities` and `@pattern` header tags. An `@exposure` outside
/// [`EXPOSURES`], an empty `@capabilities` and a `@pattern` that is not a kebab-case pattern type are
/// errors, not silent defaults.
pub fn header_tags(lolo_text: &str) -> Result<HeaderTags, String> {
    let mut tags = HeaderTags::default();
    for line in header_lines(lolo_text) {
        if tags.exposure.is_none() {
            if let Some(v) = tag_rest(line, "exposure").and_then(lower_word) {
                if !EXPOSURES.contains(&v) {
                    return Err(format!(
                        "invalid @exposure value \"{v}\" - expected one of {}",
                        EXPOSURES.join(" | ")
                    ));
                }
                tags.exposure = Some(v.to_string());
            }
        }
        if tags.pattern.is_none() {
            if let Some(v) = tag_rest(line, "pattern").and_then(quoted) {
                if !is_kebab(v) {
                    return Err(format!(
                        "invalid @pattern value \"{v}\" - expected a kebab-case @almadar/ui pattern type"
                    ));
                }
                tags.pattern = Some(v.to_string());
            }
        }
        if tags.capabilities.is_none() {
            if let Some(v) = tag_rest(line, "capabilities").and_then(quoted) {
                if v.is_empty() {
                    return Err(
                        "empty @capabilities value - expected a comma-separated phrase list"
                            .to_string(),
                    );
                }
                tags.capabilities = Some(v.to_string());
            }
        }
    }
    Ok(tags)
}

/// The `;;` comment block after a `.lolo` app header (after its `[ ... ]` modifier bracket, or in the
/// pre-bracket form its description line) and before the first `orbital`, joined into one paragraph.
pub fn leading_header_comments(text: &str) -> Option<String> {
    let mut comments: Vec<String> = Vec::new();
    let mut header_done = false;
    let mut in_bracket = false;
    for raw in text.lines() {
        let line = raw.trim();
        if !header_done {
            if in_bracket {
                if line == "]" {
                    header_done = true;
                }
            } else if line.starts_with("app ") && line.ends_with('[') {
                in_bracket = true;
            } else if (line.starts_with("app ") && line.ends_with(']'))
                || (line.len() >= 2 && line.starts_with('"') && line.ends_with('"'))
            {
                header_done = true;
            }
            continue;
        }
        if line.starts_with("orbital ") || line.starts_with("app ") {
            break;
        }
        if let Some(c) = line.strip_prefix(";;") {
            let c = c.trim();
            if !c.is_empty() {
                comments.push(c.to_string());
            }
        } else if !line.is_empty() && !line.starts_with('"') {
            break;
        }
    }
    if comments.is_empty() {
        return None;
    }
    Some(
        comments
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// A behavior's description: the program's own unless it is the auto-stub `"<name> as a Function"`,
/// else the `.lolo` leading comment block, else a line built from level, topic and state pattern.
pub fn build_description(
    name: &str,
    program_description: &str,
    lolo_text: Option<&str>,
    level: Level,
    topic: &str,
    state_pattern: &str,
) -> String {
    if !program_description.is_empty() && program_description != format!("{name} as a Function") {
        return program_description.to_string();
    }
    if let Some(comments) = lolo_text.and_then(leading_header_comments) {
        return comments;
    }
    let states = if state_pattern.is_empty() {
        "<no states>"
    } else {
        state_pattern
    };
    format!("{topic} {} — {name} (states: {states})", level.as_str())
}

// ---------------------------------------------------------------------------
// exposure
// ---------------------------------------------------------------------------

/// The behavior packages whose exposure defaults are declared here. Any other package exposes only
/// what its `.lolo` header tags say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageSource {
    Std,
    Io,
}

impl PackageSource {
    pub fn of(package_name: &str) -> Option<Self> {
        match package_name {
            "@almadar/std" => Some(Self::Std),
            "@almadar-io/behaviors" => Some(Self::Io),
            _ => None,
        }
    }
}

/// `io` topics exposed at all; every other io topic is `internal`.
const IO_EXPOSED_TOPICS: [&str; 5] = ["app", "learning", "marketing", "experience", "solutions"];
/// Non-ui organism topics that default to `app`, per package.
const IO_APP_ORGANISM_TOPICS: [&str; 4] = ["app", "learning", "marketing", "solutions"];

/// A behavior's exposure. Order, first match wins: an explicit `@exposure` tag; `templates` is
/// `internal`; an `io` topic outside the exposed set is `internal`; a `.hand-authored.json` member is
/// `both`; the `ui` substrate is `palette`; non-ui atoms and molecules are `both`; non-ui organisms
/// default to `experience`/`app` in the topics that declare it, else `internal`. A package without
/// declared defaults answers only its tags.
pub fn resolve_exposure(
    file: &IndexedFile,
    source: Option<PackageSource>,
    hand_authored: bool,
    explicit: Option<&str>,
) -> Option<String> {
    if let Some(e) = explicit {
        return Some(e.to_string());
    }
    let tier = file.tier();
    if tier == "templates" {
        return Some("internal".to_string());
    }
    let topic = file.tiered_topic();
    let head = topic.first().map(String::as_str).unwrap_or("");
    let is_ui = head == "ui";
    let source = source?;
    let answer = if source == PackageSource::Io && !IO_EXPOSED_TOPICS.contains(&head) {
        "internal"
    } else if hand_authored {
        "both"
    } else if is_ui {
        "palette"
    } else if tier == "atoms" || tier == "molecules" {
        "both"
    } else if source == PackageSource::Io && head == "experience" {
        "experience"
    } else if source == PackageSource::Io && IO_APP_ORGANISM_TOPICS.contains(&head) {
        "app"
    } else {
        "internal"
    };
    Some(answer.to_string())
}

/// The behaviors the `.hand-authored.json` files under `lolo_root` bless, by file stem of their `board`.
pub fn hand_authored_behaviors(lolo_root: &Path) -> Result<HashSet<String>, String> {
    fn walk(dir: &Path, out: &mut HashSet<String>) -> Result<(), String> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.file_name().is_some_and(|n| n == HAND_AUTHORED_FILE) {
                let raw = fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                let parsed: BTreeMap<String, serde_json::Value> = serde_json::from_str(&raw)
                    .map_err(|e| format!("{} is not a JSON object: {e}", path.display()))?;
                for entry in parsed.values() {
                    if let Some(board) = entry.get("board").and_then(|b| b.as_str()) {
                        if let Some(stem) = Path::new(board).file_stem().and_then(|s| s.to_str()) {
                            out.insert(stem.to_string());
                        }
                    }
                }
            }
        }
        Ok(())
    }
    let mut out = HashSet::new();
    walk(lolo_root, &mut out)?;
    Ok(out)
}

/// The `.lolo` source of `file`, when `pkg` ships one beside its registry: the same relative path
/// under `orb.source` with the `.lolo` extension.
pub fn lolo_path_of(pkg: &BehaviorPackage, file: &IndexedFile) -> Option<PathBuf> {
    let lolo_root = pkg.lolo_source.as_ref()?;
    let mut path = lolo_root.clone();
    path.extend(&file.dirs);
    path.push(format!("{}.lolo", file.name));
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dirs: &[&str]) -> IndexedFile {
        IndexedFile {
            name: "x".into(),
            path: PathBuf::from("x"),
            dirs: dirs.iter().map(|d| d.to_string()).collect(),
        }
    }

    #[test]
    fn topic_is_the_first_directory_and_ui_takes_its_domain() {
        assert_eq!(file(&["app", "organisms"]).topic(), "app");
        assert_eq!(file(&["ui", "core", "atoms"]).topic(), "ui/core");
        assert_eq!(file(&["ui"]).topic(), "ui");
        assert_eq!(file(&["websites", "atoms", "blog"]).topic(), "websites");
        assert_eq!(file(&["agent", "evals"]).topic(), "agent");
        assert_eq!(file(&[]).topic(), "");
    }

    #[test]
    fn level_follows_only_the_direct_parent() {
        assert_eq!(file(&["a", "molecules"]).level(), Level::Molecule);
        assert_eq!(file(&["a", "organisms"]).level(), Level::Organism);
        assert_eq!(file(&["a", "atoms"]).level(), Level::Atom);
        assert_eq!(file(&["a", "organisms", "blog"]).level(), Level::Atom);
        assert_eq!(file(&["a", "templates"]).level(), Level::Atom);
        assert_eq!(file(&["a", "templates"]).tier(), "templates");
        assert_eq!(file(&["a", "organisms", "blog"]).tier(), "atoms");
    }

    #[test]
    fn tags_read_only_from_the_header_and_validate() {
        let text = "app x [\n]\n;; @exposure internal\n;; @capabilities \"a, b\"\norbital O {\n;; @exposure app\n";
        let t = header_tags(text).unwrap();
        assert_eq!(t.exposure.as_deref(), Some("internal"));
        assert_eq!(t.capabilities.as_deref(), Some("a, b"));
        let quoted = header_tags(";; @exposure \"both\"\n").unwrap();
        assert_eq!(quoted.exposure.as_deref(), Some("both"));
        assert!(header_tags(";; @exposure nope\n").is_err());
        assert!(header_tags(";; @capabilities \"\"\n").is_err());
        assert_eq!(
            header_tags(";; @pattern \"data-grid\"\n").unwrap().pattern.as_deref(),
            Some("data-grid")
        );
        assert!(header_tags(";; @pattern \"Data Grid\"\n").is_err());
        assert!(header_tags(";; @pattern \"a--b\"\n").is_err());
        assert_eq!(header_tags(";; @pattern data-grid\n").unwrap().pattern, None);
        assert_eq!(
            header_tags("orbital O {\n;; @exposure app\n").unwrap(),
            HeaderTags::default()
        );
        assert_eq!(
            header_tags("orbitalish\n;; @exposure app\n")
                .unwrap()
                .exposure
                .as_deref(),
            Some("app")
        );
        assert_eq!(header_tags(";; @exposurex app\n").unwrap(), HeaderTags::default());
        assert_eq!(header_tags(";; @exposure Bad1\n").unwrap(), HeaderTags::default());
    }

    #[test]
    fn leading_comments_follow_the_bracket_and_stop_at_the_first_orbital() {
        let text = "app a [\n  version: \"1\"\n]\n;; first line\n;; second   line\n\norbital O {\n;; no\n";
        assert_eq!(
            leading_header_comments(text).as_deref(),
            Some("first line second line")
        );
        assert_eq!(leading_header_comments("app a [\n]\norbital O {\n"), None);
        let old = "app a ]\n;; only\norbital O {\n";
        assert_eq!(leading_header_comments(old).as_deref(), Some("only"));
        let broken = "app a [\n]\nsomething\n;; late\n";
        assert_eq!(leading_header_comments(broken), None);
    }

    #[test]
    fn description_prefers_the_program_then_comments_then_a_built_line() {
        let d = |prog: &str, lolo: Option<&str>, sp: &str| {
            build_description("n", prog, lolo, Level::Organism, "app", sp)
        };
        assert_eq!(d("Real.", None, ""), "Real.");
        assert_eq!(d("n as a Function", Some("app a [\n]\n;; hi\n"), ""), "hi");
        assert_eq!(d("n as a Function - more", None, ""), "n as a Function - more");
        assert_eq!(d("", None, "a, b"), "app organism — n (states: a, b)");
        assert_eq!(d("", None, ""), "app organism — n (states: <no states>)");
    }

    #[test]
    fn exposure_follows_the_ordered_defaults() {
        let std = Some(PackageSource::Std);
        let io = Some(PackageSource::Io);
        let r = |f: &IndexedFile, s, h, e| resolve_exposure(f, s, h, e);
        assert_eq!(r(&file(&["app", "organisms"]), io, false, Some("both")).as_deref(), Some("both"));
        assert_eq!(r(&file(&["app", "templates"]), io, false, None).as_deref(), Some("internal"));
        assert_eq!(r(&file(&["game", "atoms"]), io, true, None).as_deref(), Some("internal"));
        assert_eq!(r(&file(&["ui", "core", "atoms"]), std, true, None).as_deref(), Some("both"));
        assert_eq!(r(&file(&["ui", "core", "atoms"]), std, false, None).as_deref(), Some("palette"));
        assert_eq!(r(&file(&["agent", "atoms"]), std, false, None).as_deref(), Some("both"));
        assert_eq!(r(&file(&["agent", "organisms"]), std, false, None).as_deref(), Some("internal"));
        assert_eq!(r(&file(&["app", "organisms"]), io, false, None).as_deref(), Some("app"));
        assert_eq!(r(&file(&["experience", "organisms"]), io, false, None).as_deref(), Some("experience"));
        assert_eq!(r(&file(&["learning", "atoms"]), io, false, None).as_deref(), Some("both"));
        assert_eq!(r(&file(&["curated", "organisms"]), io, false, None).as_deref(), Some("internal"));
        assert_eq!(r(&file(&["agent", "atoms"]), None, false, None), None);
        assert_eq!(r(&file(&["agent", "atoms"]), None, false, Some("app")).as_deref(), Some("app"));
        assert_eq!(r(&file(&["websites", "atoms", "blog"]), io, false, None).as_deref(), Some("internal"));
    }

    #[test]
    fn hand_authored_boards_are_read_by_file_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ui/core");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(".hand-authored.json"),
            r#"{"k":{"signature":"s","board":"packages/p/behaviors/lolo/ui/core/atoms/std-board.lolo"}}"#,
        )
        .unwrap();
        let found = hand_authored_behaviors(tmp.path()).unwrap();
        assert!(found.contains("std-board"));
        assert_eq!(found.len(), 1);
        assert!(hand_authored_behaviors(&tmp.path().join("absent")).unwrap().is_empty());
        fs::write(dir.join(".hand-authored.json"), "[").unwrap();
        assert!(hand_authored_behaviors(tmp.path()).is_err());
    }
}
