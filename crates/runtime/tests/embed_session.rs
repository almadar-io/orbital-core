//! `embed::EmbeddedKernel` replays the scripted sessions `orbital-ffi` pins through its C ABI and
//! must produce the same wire JSON (`tests/fixtures/embed/golden`, recorded from the C ABI). A host
//! failure is `{"error": msg}` and the last one is what `lastError` reads, as the C ABI reports it.

use orbital_core_runtime::embed::{parse_theme_mode, EmbeddedKernel};
use orbital_core_runtime::kernel::TraitSnapshot;
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value as CoreValue};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn embed_dir() -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("tests/fixtures/embed")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

struct Session {
    kernel: EmbeddedKernel,
    last_error: String,
}

impl Session {
    fn report<T: serde::Serialize>(&mut self, result: Result<T, String>) -> Value {
        match result {
            Ok(value) => serde_json::to_value(value).unwrap(),
            Err(e) => {
                self.last_error = e.clone();
                json!({ "error": e })
            }
        }
    }

    fn note(&mut self, result: Result<(), String>) -> Value {
        if let Err(e) = result {
            self.last_error = e;
        }
        Value::Null
    }

    fn step(&mut self, step: &Value) -> Value {
        let s = |key: &str| step[key].as_str().unwrap().to_string();
        let entity = step
            .get("entityId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let parsed = |key: &str, what: &str| -> Result<CoreValue, String> {
            serde_json::from_value(step[key].clone()).map_err(|e| format!("parse {what}: {e}"))
        };
        match step["op"].as_str().unwrap() {
            "dispatch" => {
                let payload: CoreValue = step
                    .get("payload")
                    .map(|p| serde_json::from_value(p.clone()).unwrap())
                    .unwrap_or(CoreValue::Null);
                let result = self
                    .kernel
                    .owning_orbital(&s("trait"))
                    .map(|_| ())
                    .and_then(|_| {
                        self.kernel
                            .dispatch(&s("trait"), entity.as_deref(), &s("event"), payload)
                    });
                self.report(result)
            }
            "apply" => {
                let response: EventResponse = serde_json::from_value(step["response"].clone())
                    .map_err(|e| format!("parse response_json: {e}"))
                    .unwrap();
                let already: Vec<(String, String)> =
                    serde_json::from_value(step["alreadyDelivered"].clone()).unwrap();
                let already: HashSet<(String, String)> = already.into_iter().collect();
                let result = self.kernel.apply_server_response(&response, &already);
                self.report(result)
            }
            "restore" => {
                let snapshot: Result<TraitSnapshot, String> =
                    serde_json::from_value(step["snapshot"].clone())
                        .map_err(|e| format!("parse snapshot_json: {e}"));
                let result =
                    snapshot.map(|snap| self.kernel.restore(&s("trait"), entity.as_deref(), snap));
                self.report(result)
            }
            "abandon" => Value::Null,
            "tick" => {
                let result = self.kernel.tick(&s("trait"), &s("tick"), entity.as_deref());
                self.report(result)
            }
            "frame" => match self.kernel.frame(step["nowMs"].as_u64().unwrap()) {
                Ok(None) => Value::Null,
                Ok(Some(frame)) => {
                    if let Some(first) = frame.errors.first() {
                        self.last_error = first.clone();
                    }
                    serde_json::to_value(frame).unwrap()
                }
                Err(e) => self.report::<()>(Err(e)),
            },
            "framePause" => {
                self.kernel.frame_pause();
                Value::Null
            }
            "frameResume" => {
                self.kernel.frame_resume();
                Value::Null
            }
            "tickCount" => json!(self.kernel.tick_count()),
            "getState" => json!(self.kernel.get_state(&s("trait"), entity.as_deref())),
            "snapshot" => serde_json::to_value(self.kernel.snapshot()).unwrap(),
            "setThemeMode" => {
                let result = parse_theme_mode(&s("mode")).map(|m| self.kernel.set_theme_mode(m));
                self.note(result)
            }
            "setUser" => {
                let result = parsed("user", "user_json").map(|u| self.kernel.set_current_user(u));
                self.note(result)
            }
            "seed" => {
                let result = serde_json::from_value::<Vec<CoreValue>>(step["rows"].clone())
                    .map_err(|e| format!("parse rows_json: {e}"))
                    .and_then(|rows| self.kernel.seed(&s("entityType"), rows));
                self.note(result)
            }
            "remount" | "mount" => {
                let traits: Vec<String> = serde_json::from_value(step["traits"].clone()).unwrap();
                if step["op"] == "mount" {
                    self.kernel.mount(traits);
                } else {
                    self.kernel.remount(traits);
                }
                Value::Null
            }
            "lastError" => json!(self.last_error),
            other => panic!("unknown op {other}"),
        }
    }
}

fn run_script(script: &Value) -> Vec<Value> {
    let schema: OrbitalSchema = serde_json::from_value(read_json(
        &embed_dir()
            .join("resolved")
            .join(format!("{}.json", script["schema"].as_str().unwrap())),
    ))
    .unwrap();
    let kernel = EmbeddedKernel::new(schema, script["clientId"].as_str().unwrap()).unwrap();
    let mut session = Session {
        kernel,
        last_error: String::new(),
    };
    script["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| session.step(step))
        .collect()
}

#[test]
fn every_script_matches_the_c_abi_golden() {
    let mut scripts: Vec<PathBuf> = std::fs::read_dir(embed_dir().join("scripts"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    scripts.sort();
    assert_eq!(scripts.len(), 6);
    for path in scripts {
        let script = read_json(&path);
        let expected = read_json(&embed_dir().join("golden").join(path.file_name().unwrap()));
        let outputs = run_script(&script);
        let expected = expected.as_array().unwrap();
        assert_eq!(outputs.len(), expected.len(), "{}", path.display());
        for (i, (got, want)) in outputs.iter().zip(expected).enumerate() {
            assert_eq!(
                got,
                want,
                "{} step {i} {}",
                path.display(),
                script["steps"][i]
            );
        }
    }
}

#[test]
fn control_a_changed_step_breaks_the_golden() {
    let mut script = read_json(&embed_dir().join("scripts/rollback.json"));
    let golden = read_json(&embed_dir().join("golden/rollback.json"));
    script["steps"][2]["event"] = json!("INIT");
    assert_ne!(Value::Array(run_script(&script)), golden);
}

#[test]
fn an_empty_schema_is_refused() {
    let schema: OrbitalSchema =
        serde_json::from_value(json!({ "name": "Empty", "orbitals": [] })).unwrap();
    assert_eq!(
        EmbeddedKernel::new(schema, "c").err().as_deref(),
        Some("resolved schema declares no orbitals")
    );
}
