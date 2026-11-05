//! The shell program protocol. Each shell ships as its own executable
//! (`orb-shell-<name>`); `orb` resolves a program to IR and hands it over:
//!
//! - `orb-shell-<name> describe` prints a [`ShellDescription`] as JSON.
//! - `orb-shell-<name> generate --out <dir>` reads a [`GenerateRequest`] as JSON
//!   on stdin, writes the project and prints a [`ShellReply`] as JSON.
//! - `orb-shell-<name> emit` reads a [`GenerateRequest`] on stdin and prints an
//!   [`EmitReply`]: the generated files in memory, nothing written (`orb emit`).
//!
//! [`IR_VERSION`] is the one contract between `orb` and its shells: a shell
//! accepts a range of IR versions and refuses a request outside it.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::ir::types::OirModule;
use crate::project::WriteOptions;
use crate::schema::types::OrbitalSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// The IR version this `orbital-core` emits. Bump it on any IR change that
/// can break a shell built against the previous version.
pub const IR_VERSION: u32 = 2;

pub const CODE_IR_INCOMPATIBLE: &str = "ORB_SHELL_IR_INCOMPATIBLE";
pub const CODE_BAD_REQUEST: &str = "ORB_SHELL_BAD_REQUEST";
pub const CODE_GENERATE_FAILED: &str = "ORB_SHELL_GENERATE_FAILED";
pub const CODE_NOT_RUNNABLE: &str = "ORB_SHELL_NOT_RUNNABLE";
pub const CODE_NOT_INSTALLED: &str = "ORB_SHELL_NOT_INSTALLED";
pub const CODE_EMIT_UNSUPPORTED: &str = "ORB_SHELL_EMIT_UNSUPPORTED";

/// The IR versions a shell accepts, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrVersions {
    pub min: u32,
    pub max: u32,
}

impl IrVersions {
    pub fn accepts(&self, version: u32) -> bool {
        self.min <= version && version <= self.max
    }
}

/// One shell option (`--option name=value` on `orb compile`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellOption {
    pub name: String,
    pub description: String,
    /// The allowed values; `None` takes any string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellDescription {
    /// The `--shell` name (`typescript`, `ios`, …).
    pub name: String,
    pub version: String,
    pub ir_versions: IrVersions,
    #[serde(default)]
    pub options: Vec<ShellOption>,
    /// Whether the shell needs the resolved program alongside the IR.
    #[serde(default)]
    pub needs_resolved: bool,
    /// Whether the shell hosts a server and needs the program composed with `std-http-server` alongside the IR.
    #[serde(default)]
    pub needs_served: bool,
    /// A prebuilt web client this shell ships that serves any program from the runtime files a client-mode generate
    /// writes beside it (`orb serve`); `None` when the shell ships none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_client: Option<PathBuf>,
    /// Why the installed core/std cannot load for this shell (for example a std newer than the shell), so `orb list` and
    /// `orb update` see the mismatch before a generate fails; `None` when they load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_problem: Option<ShellFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateRequest {
    pub ir_version: u32,
    pub module: OirModule,
    /// The resolved program, sent when the shell declares `needsResolved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<OrbitalSchema>,
    /// The `uses lazy` behaviors of `resolved`, each resolved on its own (EXEC-LOAD-2), flattened: `orbRef` is relative to
    /// the folder of the program that boots them. Sent with `resolved`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lazy: Vec<LazyProgram>,
    /// The resolved program composed with `std-http-server`, sent when the shell declares `needsServed` and the mode
    /// generates a server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served: Option<OrbitalSchema>,
    /// The program's source file; project files (locales, `public/`, component
    /// mappings) are found relative to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
    pub write: WriteOptions,
    /// Shell options by name (as the shell's description declares them).
    #[serde(default)]
    pub options: BTreeMap<String, serde_json::Value>,
}

/// One lazily-used behavior of a generated program: its resolved program, at `orb_ref` beside the booting program.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LazyProgram {
    pub orb_ref: String,
    pub program: OrbitalSchema,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateResponse {
    /// Every file written, as the generated manifest lists them.
    pub files: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// "Next steps" lines for the person who ran the compile.
    #[serde(default)]
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellFailure {
    pub code: String,
    pub message: String,
}

impl ShellFailure {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ShellFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ShellFailure {}

impl From<crate::project::WriteError> for ShellFailure {
    fn from(error: crate::project::WriteError) -> Self {
        match error {
            crate::project::WriteError::Backend(message) => {
                ShellFailure::new(CODE_GENERATE_FAILED, message)
            }
            other => ShellFailure::new(CODE_GENERATE_FAILED, other.to_string()),
        }
    }
}

/// A shell's description accepting exactly this core's [`IR_VERSION`].
pub fn description(
    name: &str,
    version: &str,
    needs_resolved: bool,
    options: Vec<ShellOption>,
) -> ShellDescription {
    ShellDescription {
        name: name.to_string(),
        version: version.to_string(),
        ir_versions: IrVersions {
            min: IR_VERSION,
            max: IR_VERSION,
        },
        options,
        needs_resolved,
        needs_served: false,
        web_client: None,
        language_problem: None,
    }
}

/// The written files' manifest paths, for a [`GenerateResponse`].
pub fn written_files(output: &crate::generate::GeneratedOutput) -> Vec<String> {
    output.files.iter().map(|f| f.path.clone()).collect()
}

/// What `generate` prints: the response, or why it failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShellReply {
    Ok(GenerateResponse),
    Error(ShellFailure),
}

/// What `emit` prints: the generated files, or why there are none.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EmitReply {
    Ok(crate::generate::GeneratedOutput),
    Error(ShellFailure),
}

/// A shell executable's behavior; [`run`] wires it to argv/stdin/stdout.
pub trait ShellProgram {
    fn describe(&self) -> ShellDescription;
    /// Why the installed language data does not load for this shell; `None` for a shell that reads none.
    fn language_problem(&self) -> Option<ShellFailure> {
        None
    }
    fn generate(
        &self,
        request: &GenerateRequest,
        output: &Path,
    ) -> Result<GenerateResponse, ShellFailure>;
    /// The backend's files for `request.module`, written nowhere.
    fn emit(
        &self,
        _request: &GenerateRequest,
    ) -> Result<crate::generate::GeneratedOutput, ShellFailure> {
        Err(ShellFailure::new(
            CODE_EMIT_UNSUPPORTED,
            format!("the {} shell does not emit files", self.describe().name),
        ))
    }
}

fn check_ir(program: &dyn ShellProgram, request: &GenerateRequest) -> Result<(), ShellFailure> {
    let description = program.describe();
    if description.ir_versions.accepts(request.ir_version) {
        return Ok(());
    }
    Err(ShellFailure::new(
        CODE_IR_INCOMPATIBLE,
        format!(
            "the {} shell {} accepts IR {}..={}, the request is IR {}; update the shell (orb update) or orb",
            description.name, description.version, description.ir_versions.min, description.ir_versions.max, request.ir_version
        ),
    ))
}

/// Handle one request in-process: the IR version check, then the shell.
pub fn handle(program: &dyn ShellProgram, request: &GenerateRequest, output: &Path) -> ShellReply {
    match check_ir(program, request).and_then(|()| program.generate(request, output)) {
        Ok(response) => ShellReply::Ok(response),
        Err(failure) => ShellReply::Error(failure),
    }
}

/// [`handle`] for `emit`.
pub fn handle_emit(program: &dyn ShellProgram, request: &GenerateRequest) -> EmitReply {
    match check_ir(program, request).and_then(|()| program.emit(request)) {
        Ok(output) => EmitReply::Ok(output),
        Err(failure) => EmitReply::Error(failure),
    }
}

pub fn read_request<T: serde::de::DeserializeOwned>() -> Result<T, ShellFailure> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| ShellFailure::new(CODE_BAD_REQUEST, format!("read stdin: {e}")))?;
    from_deep_json(input.as_bytes()).map_err(|e| ShellFailure::new(CODE_BAD_REQUEST, format!("parse request: {e}")))
}

/// Parse JSON at any nesting depth: a resolved program nests as deep as its expressions.
pub fn from_deep_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    de.disable_recursion_limit();
    let value = T::deserialize(serde_stacker::Deserializer::new(&mut de))?;
    de.end()?;
    Ok(value)
}

pub fn reply_exit<T: Serialize>(reply: &T, ok: bool) -> ExitCode {
    println!(
        "{}",
        serde_json::to_string(reply).expect("a reply serializes")
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The `main` of a shell executable.
pub fn run(program: &dyn ShellProgram) -> ExitCode {
    // stdout carries the protocol; logs go to stderr under the config `orb` exported.
    crate::logging::init();
    let release: &'static str = Box::leak(program.describe().version.into_boxed_str());
    let _ = crate::language_data::register_host_release(release);
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd] if cmd == "describe" => {
            let mut description = program.describe();
            description.language_problem = program.language_problem();
            println!(
                "{}",
                serde_json::to_string(&description).expect("a description serializes")
            );
            ExitCode::SUCCESS
        }
        [cmd, flag, out] if cmd == "generate" && flag == "--out" => {
            let reply = match read_request() {
                Ok(request) => handle(program, &request, Path::new(out)),
                Err(failure) => ShellReply::Error(failure),
            };
            let ok = matches!(reply, ShellReply::Ok(_));
            reply_exit(&reply, ok)
        }
        [cmd] if cmd == "emit" => {
            let reply = match read_request() {
                Ok(request) => handle_emit(program, &request),
                Err(failure) => EmitReply::Error(failure),
            };
            let ok = matches!(reply, EmitReply::Ok(_));
            reply_exit(&reply, ok)
        }
        _ => {
            eprintln!("usage: {} describe | generate --out <dir> | emit  (generate and emit read the request JSON on stdin)", program.describe().name);
            ExitCode::from(2)
        }
    }
}

/// The npm platform suffix of this build (`darwin-arm64`, `linux-x64`, …), the
/// same names `orb`'s own platform packages use.
pub fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", _) => "darwin-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", _) => "linux-x64",
        ("windows", _) => "windows-x64",
        (os, _) => os,
    }
}

/// `orb-shell-<name>` (with `.exe` on Windows).
pub fn shell_binary(name: &str) -> String {
    format!("orb-shell-{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The npm package a first-party shell's binary ships in for this platform.
pub fn shell_package(name: &str) -> String {
    format!("@almadar/orb-shell-{name}-{}", platform())
}

/// Find the `name` shell program: `ORB_SHELL_<NAME>`, else beside `beside`
/// (orb's own executable), else the installed platform package resolved from
/// `start_dir` (nearest `node_modules` first, then `user_store`).
pub fn find_shell(
    name: &str,
    beside: Option<&Path>,
    start_dir: &Path,
    user_store: &Path,
) -> Result<PathBuf, ShellFailure> {
    let program = Program {
        binary: format!("orb-shell-{name}"),
        env_var: format!("ORB_SHELL_{}", name.to_uppercase().replace('-', "_")),
        package: format!("@almadar/orb-shell-{name}"),
        what: format!("the {name} shell"),
    };
    find_program(&program, beside, start_dir, user_store)
}

/// An executable `orb` runs: its binary name, the variable that overrides it and
/// the npm package (without the platform suffix) that ships it.
pub struct Program {
    pub binary: String,
    pub env_var: String,
    pub package: String,
    /// How a message names it ("the python shell").
    pub what: String,
}

/// Find `program`: its variable, else beside `beside`, else its installed
/// platform package resolved from `start_dir` (nearest `node_modules` first,
/// then `user_store`).
pub fn find_program(
    program: &Program,
    beside: Option<&Path>,
    start_dir: &Path,
    user_store: &Path,
) -> Result<PathBuf, ShellFailure> {
    if let Ok(path) = std::env::var(&program.env_var) {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(ShellFailure::new(
                CODE_NOT_INSTALLED,
                format!(
                    "{} points at {}, which is not a file",
                    program.env_var,
                    path.display()
                ),
            ))
        };
    }
    let file = format!("{}{}", program.binary, std::env::consts::EXE_SUFFIX);
    if let Some(dir) = beside {
        let path = dir.join(&file);
        if path.is_file() {
            return Ok(path);
        }
    }
    let package = format!("{}-{}", program.package, platform());
    // A dependency resolves from its dependent's real location (pnpm keeps the platform
    // package beside the meta package inside its store, not at the project's top level).
    let meta = crate::behavior_packages::find_npm_package(&program.package, start_dir, user_store);
    let platform_package = meta
        .and_then(|meta| {
            crate::behavior_packages::find_npm_package(&package, &meta.root, user_store)
        })
        .or_else(|| crate::behavior_packages::find_npm_package(&package, start_dir, user_store));
    if let Some(installed) = platform_package {
        return installed
            .bin
            .get(&program.binary)
            .filter(|p| p.is_file())
            .cloned()
            .ok_or_else(|| {
                ShellFailure::new(
                    CODE_NOT_INSTALLED,
                    format!(
                        "{package} at {} declares no {} binary",
                        installed.root.display(),
                        program.binary
                    ),
                )
            });
    }
    Err(ShellFailure::new(
        CODE_NOT_INSTALLED,
        format!(
            "{} is not installed (no {package}); run `orb add {}`",
            program.what, program.package
        ),
    ))
}

/// `orb`'s side: run `binary describe`.
pub fn describe(binary: &Path) -> Result<ShellDescription, ShellFailure> {
    let output = Command::new(binary)
        .arg("describe")
        .output()
        .map_err(|e| ShellFailure::new(CODE_NOT_RUNNABLE, format!("{}: {e}", binary.display())))?;
    if !output.status.success() {
        return Err(ShellFailure::new(
            CODE_NOT_RUNNABLE,
            format!(
                "{} describe exited {}: {}",
                binary.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| {
        ShellFailure::new(
            CODE_NOT_RUNNABLE,
            format!("{} describe printed no description: {e}", binary.display()),
        )
    })
}

/// Run `binary <args>` with `request` on stdin and parse its JSON reply.
pub fn exchange<Q: Serialize, R: serde::de::DeserializeOwned>(
    binary: &Path,
    args: &[&std::ffi::OsStr],
    request: &Q,
) -> Result<R, ShellFailure> {
    let payload = serde_json::to_vec(request)
        .map_err(|e| ShellFailure::new(CODE_BAD_REQUEST, e.to_string()))?;
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| ShellFailure::new(CODE_NOT_RUNNABLE, format!("{}: {e}", binary.display())))?;
    {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(&payload).map_err(|e| {
            ShellFailure::new(
                CODE_NOT_RUNNABLE,
                format!("write request to {}: {e}", binary.display()),
            )
        })?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| ShellFailure::new(CODE_NOT_RUNNABLE, format!("{}: {e}", binary.display())))?;
    from_deep_json(&out.stdout).map_err(|e| {
        ShellFailure::new(
            CODE_NOT_RUNNABLE,
            format!(
                "{} exited {} without a reply: {e}",
                binary.display(),
                out.status
            ),
        )
    })
}

/// `orb`'s side: run `binary generate --out <output>` with `request` on stdin.
pub fn invoke(
    binary: &Path,
    request: &GenerateRequest,
    output: &Path,
) -> Result<GenerateResponse, ShellFailure> {
    match exchange(
        binary,
        &["generate".as_ref(), "--out".as_ref(), output.as_os_str()],
        request,
    )? {
        ShellReply::Ok(response) => Ok(response),
        ShellReply::Error(failure) => Err(failure),
    }
}

/// `orb`'s side: run `binary emit` with `request` on stdin.
pub fn invoke_emit(
    binary: &Path,
    request: &GenerateRequest,
) -> Result<crate::generate::GeneratedOutput, ShellFailure> {
    match exchange(binary, &["emit".as_ref()], request)? {
        EmitReply::Ok(output) => Ok(output),
        EmitReply::Error(failure) => Err(failure),
    }
}

/// Copy the native kernel add-on `file` that ships beside the running shell program into `<output>/kernel/`, where a
/// kernel-hosted project loads it. Its absence is the project's note (the app then needs it in `kernel/` or
/// `ORBITAL_KERNEL_DIR`), not a failure.
pub fn ship_kernel_beside_program(
    file: &str,
    output: &std::path::Path,
) -> Result<Option<String>, ShellFailure> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(file)));
    let Some(source) = beside.filter(|path| path.exists()) else {
        return Ok(Some(format!(
            "{file} is not beside this shell program; put it in kernel/ or set ORBITAL_KERNEL_DIR before running the app"
        )));
    };
    let target = output.join("kernel");
    std::fs::create_dir_all(&target).map_err(|e| {
        ShellFailure::new(
            CODE_GENERATE_FAILED,
            format!("create {}: {e}", target.display()),
        )
    })?;
    std::fs::copy(&source, target.join(file)).map_err(|e| {
        ShellFailure::new(
            CODE_GENERATE_FAILED,
            format!("copy {}: {e}", source.display()),
        )
    })?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested(depth: usize) -> String {
        format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn deep_json_a_request_nested_far_past_128_levels_parses() {
        let parsed: serde_json::Value = from_deep_json(nested(4000).as_bytes()).expect("a deep request parses");
        let mut depth = 0;
        let mut at = &parsed;
        while let serde_json::Value::Array(items) = at {
            depth += 1;
            at = &items[0];
        }
        assert_eq!(depth, 4000);
        assert_eq!(at, &serde_json::json!(1));
    }

    #[test]
    fn deep_json_control_a_shallow_request_parses_as_before() {
        let parsed: serde_json::Value = from_deep_json(br#"{"a":[1,{"b":2}]}"#).expect("parses");
        assert_eq!(parsed, serde_json::json!({"a":[1,{"b":2}]}));
    }

    #[test]
    fn deep_json_edge_malformed_or_trailing_text_still_refuses() {
        assert!(from_deep_json::<serde_json::Value>(b"[[1]").is_err());
        assert!(from_deep_json::<serde_json::Value>(b"[1] trailing").is_err());
        let unclosed = format!("{}1{}", "[".repeat(4000), "]".repeat(3999));
        assert!(from_deep_json::<serde_json::Value>(unclosed.as_bytes()).is_err());
    }

    struct Fake;

    impl ShellProgram for Fake {
        fn describe(&self) -> ShellDescription {
            ShellDescription {
                name: "fake".into(),
                version: "1.0.0".into(),
                ir_versions: IrVersions {
                    min: IR_VERSION,
                    max: IR_VERSION,
                },
                options: vec![],
                needs_resolved: false,
                needs_served: false,
                web_client: None,
                language_problem: None,
            }
        }
        fn generate(
            &self,
            request: &GenerateRequest,
            _output: &Path,
        ) -> Result<GenerateResponse, ShellFailure> {
            Ok(GenerateResponse {
                files: vec![format!("{}.txt", request.module.name)],
                ..Default::default()
            })
        }
    }

    fn request(ir_version: u32) -> GenerateRequest {
        GenerateRequest {
            ir_version,
            module: OirModule::new("App", "1.0.0"),
            resolved: None,
            lazy: Vec::new(),
            served: None,
            source_path: None,
            write: WriteOptions::default(),
            options: BTreeMap::new(),
        }
    }

    struct Emitter;

    impl ShellProgram for Emitter {
        fn describe(&self) -> ShellDescription {
            Fake.describe()
        }
        fn generate(
            &self,
            request: &GenerateRequest,
            output: &Path,
        ) -> Result<GenerateResponse, ShellFailure> {
            Fake.generate(request, output)
        }
        fn emit(
            &self,
            request: &GenerateRequest,
        ) -> Result<crate::generate::GeneratedOutput, ShellFailure> {
            Ok(crate::generate::GeneratedOutput {
                files: vec![crate::generate::GeneratedFile::new(
                    format!("{}.txt", request.module.name),
                    "hi",
                )],
                warnings: vec![],
            })
        }
    }

    #[test]
    fn a_language_problem_rides_the_description_and_a_loading_install_omits_it() {
        let mut broken = Fake.describe();
        broken.language_problem = Some(ShellFailure::new(
            "ORB_LANGUAGE_DATA_INCOMPATIBLE",
            "@almadar/std@16.256.0 names csv/parse",
        ));
        let json = serde_json::to_value(&broken).unwrap();
        assert_eq!(
            json["languageProblem"]["code"],
            "ORB_LANGUAGE_DATA_INCOMPATIBLE"
        );
        assert_eq!(
            serde_json::from_value::<ShellDescription>(json).unwrap(),
            broken
        );

        let json = serde_json::to_value(Fake.describe()).unwrap();
        assert!(
            json.get("languageProblem").is_none(),
            "control: nothing wrong reports nothing"
        );
        let older = serde_json::json!({"name": "old", "version": "1.0.0", "irVersions": {"min": 1, "max": 1}});
        assert_eq!(
            serde_json::from_value::<ShellDescription>(older)
                .unwrap()
                .language_problem,
            None
        );
    }

    #[test]
    fn a_shipped_web_client_rides_the_description_and_an_older_description_has_none() {
        let mut shipping = Fake.describe();
        shipping.web_client = Some(PathBuf::from("/opt/shell/templates/almadar-web-client"));
        let json = serde_json::to_value(&shipping).unwrap();
        assert_eq!(json["webClient"], "/opt/shell/templates/almadar-web-client");
        assert_eq!(
            serde_json::from_value::<ShellDescription>(json).unwrap(),
            shipping
        );
        let older = serde_json::json!({ "name": "fake", "version": "1.0.0", "irVersions": { "min": 1, "max": 1 } });
        assert_eq!(
            serde_json::from_value::<ShellDescription>(older)
                .unwrap()
                .web_client,
            None
        );
        assert!(serde_json::to_value(Fake.describe())
            .unwrap()
            .get("webClient")
            .is_none());
    }

    #[test]
    fn emit_returns_the_files_in_memory() {
        let EmitReply::Ok(output) = handle_emit(&Emitter, &request(IR_VERSION)) else {
            panic!("expected files")
        };
        assert_eq!(
            output.files,
            vec![crate::generate::GeneratedFile::new("App.txt", "hi")]
        );
        let json = serde_json::to_string(&EmitReply::Ok(output)).unwrap();
        assert!(
            matches!(serde_json::from_str::<EmitReply>(&json).unwrap(), EmitReply::Ok(o) if o.files[0].content == "hi")
        );
    }

    #[test]
    fn control_a_shell_without_emit_refuses_it_by_code() {
        let EmitReply::Error(failure) = handle_emit(&Fake, &request(IR_VERSION)) else {
            panic!("expected a refusal")
        };
        assert_eq!(failure.code, CODE_EMIT_UNSUPPORTED);
    }

    #[test]
    fn edge_emit_checks_the_ir_version_first() {
        let EmitReply::Error(failure) = handle_emit(&Emitter, &request(IR_VERSION + 1)) else {
            panic!("expected a refusal")
        };
        assert_eq!(failure.code, CODE_IR_INCOMPATIBLE);
    }

    #[test]
    fn a_request_in_the_accepted_range_reaches_the_shell() {
        assert_eq!(
            handle(&Fake, &request(IR_VERSION), Path::new("/tmp")),
            ShellReply::Ok(GenerateResponse {
                files: vec!["App.txt".into()],
                ..Default::default()
            })
        );
    }

    #[test]
    fn control_a_request_outside_the_range_is_refused_before_the_shell_runs() {
        let ShellReply::Error(failure) = handle(&Fake, &request(IR_VERSION + 1), Path::new("/tmp"))
        else {
            panic!("expected a refusal");
        };
        assert_eq!(failure.code, CODE_IR_INCOMPATIBLE);
        assert!(
            failure
                .message
                .contains(&format!("accepts IR {IR_VERSION}..={IR_VERSION}")),
            "{}",
            failure.message
        );
    }

    #[test]
    fn a_shell_built_for_the_previous_request_contract_is_rejected() {
        struct PreviousShell;
        impl ShellProgram for PreviousShell {
            fn describe(&self) -> ShellDescription {
                let mut description = Fake.describe();
                description.ir_versions = IrVersions {
                    min: IR_VERSION - 1,
                    max: IR_VERSION - 1,
                };
                description
            }
            fn generate(
                &self,
                _request: &GenerateRequest,
                _output: &Path,
            ) -> Result<GenerateResponse, ShellFailure> {
                panic!("old shell must not receive the new request")
            }
        }
        let ShellReply::Error(failure) =
            handle(&PreviousShell, &request(IR_VERSION), Path::new("/tmp"))
        else {
            panic!("expected a refusal");
        };
        assert_eq!(failure.code, CODE_IR_INCOMPATIBLE);
    }

    #[test]
    fn the_request_and_reply_round_trip_as_json() {
        let json = serde_json::to_string(&request(IR_VERSION)).unwrap();
        let back: GenerateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ir_version, IR_VERSION);
        assert_eq!(back.module.name, "App");
        let reply = ShellReply::Error(ShellFailure::new(CODE_GENERATE_FAILED, "boom"));
        assert_eq!(
            serde_json::from_str::<ShellReply>(&serde_json::to_string(&reply).unwrap()).unwrap(),
            reply
        );
    }

    fn installed(root: &Path, name: &str) -> PathBuf {
        let pkg = root.join("node_modules").join(shell_package(name));
        std::fs::create_dir_all(pkg.join("bin")).unwrap();
        let binary = pkg.join("bin").join(format!("orb-shell-{name}"));
        std::fs::write(&binary, "").unwrap();
        std::fs::write(
            pkg.join("package.json"),
            serde_json::json!({"name": shell_package(name), "version": "1.0.0", "bin": {format!("orb-shell-{name}"): format!("bin/orb-shell-{name}")}}).to_string(),
        )
        .unwrap();
        binary
    }

    #[test]
    fn an_installed_platform_package_provides_the_shell() {
        let root = std::env::temp_dir().join(format!("orb-find-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let binary = installed(&root, "zzfixture");
        let found = find_shell("zzfixture", None, &root, &root.join("no-store")).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            binary.canonicalize().unwrap()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_shell_beside_the_running_binary_wins_over_an_installed_one() {
        let root = std::env::temp_dir().join(format!("orb-find-beside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        installed(&root, "zzbeside");
        let beside = root.join("bin");
        std::fs::create_dir_all(&beside).unwrap();
        std::fs::write(beside.join(shell_binary("zzbeside")), "").unwrap();
        assert_eq!(
            find_shell("zzbeside", Some(&beside), &root, &root).unwrap(),
            beside.join(shell_binary("zzbeside"))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    fn pnpm_project(root: &Path, name: &str, link_meta: bool) -> PathBuf {
        let store = root
            .join("node_modules/.pnpm")
            .join(format!("orb-shell-{name}@1.0.0"));
        let binary = installed(&store, name);
        let meta = store
            .join("node_modules/@almadar")
            .join(format!("orb-shell-{name}"));
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("package.json"),
            serde_json::json!({"name": format!("@almadar/orb-shell-{name}"), "version": "1.0.0"})
                .to_string(),
        )
        .unwrap();
        if link_meta {
            std::fs::create_dir_all(root.join("node_modules/@almadar")).unwrap();
            std::os::unix::fs::symlink(
                &meta,
                root.join("node_modules/@almadar")
                    .join(format!("orb-shell-{name}")),
            )
            .unwrap();
        }
        binary
    }

    #[cfg(unix)]
    #[test]
    fn a_platform_package_resolves_from_its_meta_package_in_a_pnpm_project() {
        let root = std::env::temp_dir().join(format!("orb-find-pnpm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let binary = pnpm_project(&root, "zzpnpm", true);
        let found = find_shell("zzpnpm", None, &root, &root.join("no-store")).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            binary.canonicalize().unwrap()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn control_a_platform_package_stranded_in_the_store_is_not_installed() {
        let root = std::env::temp_dir().join(format!("orb-find-stranded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        pnpm_project(&root, "zzstranded", false);
        let err = find_shell("zzstranded", None, &root, &root.join("no-store")).unwrap_err();
        assert_eq!(err.code, CODE_NOT_INSTALLED);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn control_a_missing_shell_names_the_package_to_add() {
        let root = std::env::temp_dir().join(format!("orb-find-none-{}", std::process::id()));
        let err = find_shell("zznone", None, &root, &root).unwrap_err();
        assert_eq!(err.code, CODE_NOT_INSTALLED);
        assert!(
            err.message.contains("orb add @almadar/orb-shell-zznone"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_backend_refusal_carries_the_backend_message_unwrapped() {
        let failure = ShellFailure::from(crate::project::WriteError::Backend("ORB_X: no".into()));
        assert_eq!(
            (failure.code.as_str(), failure.message.as_str()),
            (CODE_GENERATE_FAILED, "ORB_X: no")
        );
    }

    #[test]
    fn control_an_io_failure_keeps_its_kind() {
        let failure = ShellFailure::from(crate::project::WriteError::Io(std::io::Error::other(
            "disk",
        )));
        assert_eq!(failure.message, "IO error: disk");
    }

    #[test]
    fn edge_a_binary_that_does_not_exist_is_not_runnable() {
        let err = describe(Path::new("/nonexistent/orb-shell-none")).unwrap_err();
        assert_eq!(err.code, CODE_NOT_RUNNABLE);
    }
}
