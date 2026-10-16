//! The `@almadar/core` conformance fixture sets, read from the INSTALLED core
//! (`package.json` → `orbLanguage.fixtures`, the same package the language data
//! comes from — `language_data`). One owner for every consumer: this crate's
//! gates, downstream crates' wire tests and `orb verify --server`'s fixture pass
//! all read through here, never a path of their own. Loaded once; a missing or
//! malformed set panics naming the file (callers run where core is installed).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One `fixtures/events-wire/<name>/` case: every `*.json` it carries, by file name.
#[derive(Debug, Clone)]
pub struct EventWireCase {
    pub name: String,
    pub files: Vec<(String, String)>,
}

impl EventWireCase {
    pub fn file(&self, name: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, content)| content.as_str())
    }

    /// `request.json` — every case records one.
    pub fn request(&self) -> &str {
        self.file("request.json")
            .unwrap_or_else(|| panic!("events-wire case `{}` has no request.json", self.name))
    }

    /// `response.json` — every case records one.
    pub fn response(&self) -> &str {
        self.file("response.json")
            .unwrap_or_else(|| panic!("events-wire case `{}` has no response.json", self.name))
    }
}

struct Fixtures {
    event_wire_cases: Vec<EventWireCase>,
    theme_tokens_every_key: String,
    quote_cases: String,
    quasiquote_cases: String,
    behavior_apply_cases: String,
    binding_path_cases: String,
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()))
}

fn sorted_entries(dir: &Path, want_dir: bool) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("fixture dir {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            if want_dir {
                p.is_dir()
            } else {
                p.extension().and_then(|e| e.to_str()) == Some("json")
            }
        })
        .collect();
    entries.sort();
    entries
}

fn load(dir: &Path) -> Fixtures {
    let event_wire_cases = sorted_entries(&dir.join("events-wire"), true)
        .iter()
        .map(|case| EventWireCase {
            name: case.file_name().unwrap().to_string_lossy().into_owned(),
            files: sorted_entries(case, false)
                .iter()
                .map(|f| {
                    (
                        f.file_name().unwrap().to_string_lossy().into_owned(),
                        read(f),
                    )
                })
                .collect(),
        })
        .collect();
    Fixtures {
        event_wire_cases,
        theme_tokens_every_key: read(&dir.join("theme-tokens/every-key.json")),
        quote_cases: read(&dir.join("quote/cases.json")),
        quasiquote_cases: read(&dir.join("quasiquote/cases.json")),
        behavior_apply_cases: read(&dir.join("behavior-apply/cases.json")),
        binding_path_cases: read(&dir.join("binding-path/cases.json")),
    }
}

/// The installed core's declared fixture directory.
pub fn installed_fixtures_dir() -> PathBuf {
    let root = &crate::language_data::language_data_after_preflight().root;
    let manifest: serde_json::Value = serde_json::from_str(&read(&root.join("package.json")))
        .unwrap_or_else(|e| panic!("{}/package.json: {e}", root.display()));
    let rel = manifest["orbLanguage"]["fixtures"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "installed @almadar/core at {} declares no `orbLanguage.fixtures`",
                root.display()
            )
        });
    root.join(rel)
}

fn fixtures() -> &'static Fixtures {
    static FIXTURES: OnceLock<Fixtures> = OnceLock::new();
    FIXTURES.get_or_init(|| load(&installed_fixtures_dir()))
}

/// Every `fixtures/events-wire/<name>/` case, sorted by name.
pub fn event_wire_cases() -> &'static [EventWireCase] {
    &fixtures().event_wire_cases
}

/// Look one case up by its directory name.
pub fn event_wire_case(name: &str) -> Option<&'static EventWireCase> {
    event_wire_cases().iter().find(|c| c.name == name)
}

/// `fixtures/theme-tokens/every-key.json`.
pub fn theme_tokens_every_key() -> &'static str {
    &fixtures().theme_tokens_every_key
}

/// `fixtures/quote/cases.json`.
pub fn quote_cases() -> &'static str {
    &fixtures().quote_cases
}

/// `fixtures/quasiquote/cases.json`.
pub fn quasiquote_cases() -> &'static str {
    &fixtures().quasiquote_cases
}

/// `fixtures/behavior-apply/cases.json`.
pub fn behavior_apply_cases() -> &'static str {
    &fixtures().behavior_apply_cases
}

/// `fixtures/binding-path/cases.json`.
pub fn binding_path_cases() -> &'static str {
    &fixtures().binding_path_cases
}
