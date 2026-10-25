//! The desktop program protocol. The native GUI (window, Layer 2 render probe,
//! screenshots) ships as its own executable, `orb-desktop`, so `orb` links no GUI:
//!
//! - `orb-desktop request` reads a [`DesktopEnvelope`] as JSON on stdin and
//!   prints a [`DesktopReply`] as JSON.
//! - `orb-desktop window <resolved.orb> [--server <url>]` opens the app window.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::shell_protocol::{exchange, read_request, reply_exit, Program, ShellFailure, CODE_NOT_RUNNABLE};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The request shape this `orbital-core` speaks; the program refuses another.
pub const PROTOCOL_VERSION: u32 = 1;

pub const CODE_PROTOCOL_INCOMPATIBLE: &str = "ORB_DESKTOP_PROTOCOL_INCOMPATIBLE";

/// `orb-desktop`, shipped in `@almadar/orb-desktop-<platform>`, overridden by `ORB_DESKTOP`.
pub fn program() -> Program {
    Program {
        binary: "orb-desktop".to_string(),
        env_var: "ORB_DESKTOP".to_string(),
        package: "@almadar/orb-desktop".to_string(),
        what: "the desktop program".to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DesktopRequest {
    /// The patterns `renderer` draws, and the ones it stubs — what `orb`'s
    /// Layer 2 probe (`client::render_probe`) checks coverage against.
    Coverage { renderer: String },
    /// Paint one game-canvas pattern headlessly (Layer 2 `game_canvas_renders`).
    PaintCanvas { renderer: String, pattern: String, props: orbital_core_base::value::Value },
    /// One image per orbital under `out_dir`.
    Screenshot { renderer: String, item: String, schema: Box<OrbitalSchema>, out_dir: PathBuf },
    /// One image per planned transition of the richest trait, plus a `frames.json` manifest.
    Frames { renderer: String, item: String, schema: Box<OrbitalSchema>, out_dir: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopEnvelope {
    pub protocol: u32,
    pub request: DesktopRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DesktopReply {
    Coverage { supported: Vec<String>, stubbed: Vec<String> },
    Canvas(crate::client::render_probe::GameCanvasOutcome),
    /// Files written, in order.
    Files(Vec<PathBuf>),
    Error(ShellFailure),
}

/// The renderers a desktop program offers; `dioxus` is the default.
pub const RENDERERS: &[&str] = &["dioxus", "tui"];

/// The desktop executable's behavior; [`run`] wires it to argv/stdin/stdout.
pub trait DesktopProgram {
    fn handle(&self, request: DesktopRequest) -> Result<DesktopReply, ShellFailure>;
    /// Open the app window (or terminal) for a resolved program; returns when it closes.
    fn open_window(&self, renderer: &str, schema: &Path, server: Option<&str>) -> Result<(), String>;
}

/// Handle one envelope in-process: the protocol check, then the program.
pub fn handle(program: &dyn DesktopProgram, envelope: DesktopEnvelope) -> DesktopReply {
    if envelope.protocol != PROTOCOL_VERSION {
        return DesktopReply::Error(ShellFailure::new(
            CODE_PROTOCOL_INCOMPATIBLE,
            format!(
                "orb-desktop speaks protocol {PROTOCOL_VERSION}, the request is protocol {}; run `orb update`",
                envelope.protocol
            ),
        ));
    }
    program.handle(envelope.request).unwrap_or_else(DesktopReply::Error)
}

/// The `main` of `orb-desktop`.
pub fn run(program: &dyn DesktopProgram) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd] if cmd == "request" => {
            let reply = match read_request::<DesktopEnvelope>() {
                Ok(envelope) => handle(program, envelope),
                Err(failure) => DesktopReply::Error(failure),
            };
            let ok = !matches!(reply, DesktopReply::Error(_));
            reply_exit(&reply, ok)
        }
        [cmd, schema, rest @ ..] if cmd == "window" => {
            let Some((renderer, server)) = window_flags(rest) else {
                eprintln!("usage: orb-desktop window <resolved.orb> [--renderer dioxus|tui] [--server <url>]");
                return ExitCode::from(2);
            };
            match program.open_window(renderer, Path::new(schema), server) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: orb-desktop request | window <resolved.orb> [--renderer dioxus|tui] [--server <url>]  (request reads the JSON envelope on stdin)");
            ExitCode::from(2)
        }
    }
}

/// `window`'s flags: `--renderer` (default `dioxus`) and `--server`, each at most once.
fn window_flags(args: &[String]) -> Option<(&str, Option<&str>)> {
    let (mut renderer, mut server) = (None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next()?.as_str();
        let slot = match flag.as_str() {
            "--renderer" => &mut renderer,
            "--server" => &mut server,
            _ => return None,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    Some((renderer.unwrap_or(RENDERERS[0]), server))
}

/// `orb`'s side: send `request` to the program at `binary`.
pub fn request(binary: &Path, request: DesktopRequest) -> Result<DesktopReply, ShellFailure> {
    let envelope = DesktopEnvelope { protocol: PROTOCOL_VERSION, request };
    match exchange(binary, &["request".as_ref()], &envelope)? {
        DesktopReply::Error(failure) => Err(failure),
        reply => Ok(reply),
    }
}

/// `orb`'s side: open the window and wait for it to close.
pub fn open_window(binary: &Path, renderer: &str, schema: &Path, server: Option<&str>) -> Result<(), ShellFailure> {
    let mut command = Command::new(binary);
    command.arg("window").arg(schema).args(["--renderer", renderer]);
    if let Some(url) = server {
        command.args(["--server", url]);
    }
    let status = command
        .status()
        .map_err(|e| ShellFailure::new(CODE_NOT_RUNNABLE, format!("{}: {e}", binary.display())))?;
    if status.success() {
        Ok(())
    } else {
        Err(ShellFailure::new(CODE_NOT_RUNNABLE, format!("{} window exited {status}", binary.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::shell_protocol::CODE_BAD_REQUEST;

    struct Fake;

    impl DesktopProgram for Fake {
        fn handle(&self, request: DesktopRequest) -> Result<DesktopReply, ShellFailure> {
            match request {
                DesktopRequest::Coverage { renderer } if renderer == "dioxus" => {
                    Ok(DesktopReply::Coverage { supported: vec!["typography".into()], stubbed: vec!["health-bar".into()] })
                }
                _ => Err(ShellFailure::new(CODE_BAD_REQUEST, "not here")),
            }
        }
        fn open_window(&self, _renderer: &str, _schema: &Path, _server: Option<&str>) -> Result<(), String> {
            Ok(())
        }
    }

    fn coverage() -> DesktopEnvelope {
        DesktopEnvelope { protocol: PROTOCOL_VERSION, request: DesktopRequest::Coverage { renderer: "dioxus".into() } }
    }

    #[test]
    fn coverage_returns_the_renderers_lists() {
        assert!(matches!(handle(&Fake, coverage()), DesktopReply::Coverage { supported, stubbed } if supported == ["typography"] && stubbed == ["health-bar"]));
    }

    #[test]
    fn control_another_protocol_is_refused_before_the_program_runs() {
        let mut envelope = coverage();
        envelope.protocol += 1;
        let DesktopReply::Error(failure) = handle(&Fake, envelope) else { panic!("expected a refusal") };
        assert_eq!(failure.code, CODE_PROTOCOL_INCOMPATIBLE);
    }

    #[test]
    fn the_envelope_and_a_canvas_reply_round_trip_as_json() {
        let json = serde_json::to_value(coverage()).unwrap();
        assert_eq!(json["request"]["command"], "coverage");
        assert!(matches!(serde_json::from_value::<DesktopEnvelope>(json).unwrap().request, DesktopRequest::Coverage { .. }));
        let reply = DesktopReply::Canvas(crate::client::render_probe::GameCanvasOutcome::Skip("no gpu".into()));
        assert_eq!(serde_json::from_str::<DesktopReply>(&serde_json::to_string(&reply).unwrap()).unwrap(), reply);
    }

    #[test]
    fn window_flags_default_to_dioxus_and_take_each_flag_once() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(window_flags(&args(&[])), Some(("dioxus", None)));
        assert_eq!(window_flags(&args(&["--renderer", "tui", "--server", "http://x"])), Some(("tui", Some("http://x"))));
        assert_eq!(window_flags(&args(&["--renderer", "tui", "--renderer", "dioxus"])), None);
        assert_eq!(window_flags(&args(&["--renderer"])), None);
        assert_eq!(window_flags(&args(&["--nope", "x"])), None);
    }

    #[test]
    fn edge_a_program_failure_is_an_error_reply() {
        let envelope = DesktopEnvelope { protocol: PROTOCOL_VERSION, request: DesktopRequest::Coverage { renderer: "zz".into() } };
        assert!(matches!(handle(&Fake, envelope), DesktopReply::Error(f) if f.code == CODE_BAD_REQUEST));
    }

    #[test]
    fn control_a_missing_program_names_the_package_to_add() {
        let root = std::env::temp_dir().join(format!("orb-find-desktop-{}", std::process::id()));
        let err = orbital_core_base::shell_protocol::find_program(&program(), None, &root, &root).unwrap_err();
        assert!(err.message.contains("orb add @almadar/orb-desktop"), "{}", err.message);
    }
}
