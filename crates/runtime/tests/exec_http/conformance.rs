//! EXEC-HTTP, EXEC-ENDPOINT and EXEC-INBOUND conformance: the `os/tls-upgrade` ask, what mounts for each endpoint, and the inbound hook intake.

use orbital_core_base::inbound::InboundAnswer;
use orbital_core_base::os::OsOp;
use orbital_core_base::protocols::ProtocolSide;
use orbital_core_base::value::Value;
use orbital_core_runtime::driver::{DriverAnswer, DriverAsk};
use orbital_core_runtime::http_service::SecretSource;
use orbital_core_runtime::kernel::RuntimeKernel;
use orbital_core_runtime::tls::{TlsEvent, TlsStream};
use serde_json::json;

use crate::support::TestKernel;

// ---- browser/tab (EXEC-HTTP-6) ----

// ---- tls upgrade (EXEC-HTTP-7) ----

fn tls_client_hello(seed: u8) -> Vec<Value> {
    let (mut stream, first) = TlsStream::upgrade("db.internal", Value::Number(5.0));
    assert_eq!(first, TlsEvent::Ask(DriverAsk::Random { bytes: 32 }), "the seed is a journaled random ask");
    assert_eq!(stream.resume(DriverAnswer::Random(Ok(vec![seed; 32]))), TlsEvent::Ask(DriverAsk::Clock), "the time is the journaled clock");
    match stream.resume(DriverAnswer::Clock(Ok(1_760_000_000_000))) {
        TlsEvent::Ask(DriverAsk::Os { op: OsOp::Send, args, .. }) => args,
        other => panic!("the handshake begins with a raw os/send, got {other:?}"),
    }
}

/// EXEC-HTTP-7: `os/tls-upgrade` turns a connected socket into a kernel-owned TLS stream, replay-identical from its journaled seed and clock.
#[test]
fn exec_http_7_tls_upgrade_is_a_kernel_op_whose_handshake_replays_byte_identically() {
    assert_eq!(tls_client_hello(7), tls_client_hello(7), "the same seed and clock replay a byte-identical handshake");
    assert_ne!(tls_client_hello(7), tls_client_hello(8), "control: another seed is another handshake");
    assert!(OsOp::from_name("os/tls-upgrade").is_some(), "`os/tls-upgrade` is a program-callable op");
    assert!(OsOp::from_name("os/send").is_some(), "control: the raw ops it composes exist");
}

// ---- endpoints (EXEC-ENDPOINT-3, -5) ----

fn endpoint_trait(name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [] }]
        }
    })
}

fn endpoint_kernel(pages: serde_json::Value, traits: &[&str]) -> RuntimeKernel {
    TestKernel::server().build(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": traits.iter().map(|t| endpoint_trait(t)).collect::<Vec<_>>(),
            "pages": pages
        }]
    }))
}

fn mounted(seeds: Vec<orbital_core_runtime::effects::response::MountSeed>) -> Vec<String> {
    seeds.into_iter().map(|s| s.r#trait).collect()
}

fn full_app() -> RuntimeKernel {
    endpoint_kernel(
        json!([
            { "name": "Home", "path": "/", "traits": [{ "ref": "RouteT" }] },
            { "name": "Other", "path": "/other", "traits": [{ "ref": "OtherT" }] },
            { "name": "Api", "path": "", "protocol": "http", "traits": [{ "ref": "HttpT" }] },
            { "name": "Mcp", "path": "", "protocol": "stdio", "traits": [{ "ref": "StdioT" }] },
            { "name": "Bg", "path": "", "protocol": "boot", "traits": [{ "ref": "BootT" }] }
        ]),
        &["RouteT", "OtherT", "HttpT", "StdioT", "BootT", "Orphan"],
    )
}

/// EXEC-ENDPOINT-3: the kernel decides what mounts: boot, http and stdio traits at start, a route's traits when it is opened, a trait on no endpoint never.
#[test]
fn exec_endpoint_3_the_kernel_mounts_boot_http_stdio_at_start_and_a_route_only_when_opened() {
    let app = full_app();
    assert_eq!(mounted(app.boot_mounts(ProtocolSide::Server)), ["HttpT", "BootT"], "the server start mounts http and boot");
    assert_eq!(mounted(app.boot_mounts(ProtocolSide::Process)), ["StdioT", "BootT"], "the process start mounts stdio and boot");
    assert_eq!(mounted(app.route_mounts("/")), ["RouteT"], "opening a route mounts that route's traits");
    assert_eq!(mounted(app.route_mounts("/other")), ["OtherT"]);
    assert!(app.route_mounts("/missing").is_empty(), "control: a route no page declares mounts nothing");

    let mut at_start = mounted(app.boot_mounts(ProtocolSide::Server));
    at_start.extend(mounted(app.boot_mounts(ProtocolSide::Process)));
    assert!(!at_start.contains(&"RouteT".to_string()), "control: a route trait does not mount at start");
    let mut on_open = mounted(app.route_mounts("/"));
    on_open.extend(mounted(app.route_mounts("/other")));
    for protocol_trait in ["HttpT", "StdioT", "BootT"] {
        assert!(!on_open.contains(&protocol_trait.to_string()), "control: {protocol_trait} does not mount when a route opens");
    }
    let everything: Vec<String> = at_start.into_iter().chain(on_open).collect();
    assert!(!everything.contains(&"Orphan".to_string()), "a trait on no endpoint never mounts");

    let twice = endpoint_kernel(
        json!([
            { "name": "A", "path": "", "protocol": "boot", "traits": [{ "ref": "T" }] },
            { "name": "B", "path": "", "protocol": "http", "traits": [{ "ref": "T" }] }
        ]),
        &["T"],
    );
    assert_eq!(mounted(twice.boot_mounts(ProtocolSide::Server)), ["T"], "a trait on two endpoints mounts once");
}

/// EXEC-ENDPOINT-5: each shell ships the endpoints its side serves; the kernel mounts nothing a program did not declare.
#[test]
fn exec_endpoint_5_each_side_mounts_only_the_endpoints_it_serves_and_nothing_is_joined_behind_the_programs_back() {
    let app = full_app();
    assert!(app.boot_mounts(ProtocolSide::Client).is_empty(), "the client takes routes, never a start mount");
    assert_eq!(mounted(app.route_mounts("/")), ["RouteT"], "the client takes the routes");
    let server = mounted(app.boot_mounts(ProtocolSide::Server));
    let process = mounted(app.boot_mounts(ProtocolSide::Process));
    assert!(server.contains(&"HttpT".to_string()) && server.contains(&"BootT".to_string()), "the server host takes http and boot");
    assert!(!server.contains(&"StdioT".to_string()), "control: the server host does not take stdio");
    assert!(process.contains(&"StdioT".to_string()) && process.contains(&"BootT".to_string()), "a process host takes stdio and boot");
    assert!(!process.contains(&"HttpT".to_string()), "control: a process host does not take http");

    let routes_only = endpoint_kernel(
        json!([{ "name": "Home", "path": "/", "traits": [{ "ref": "RouteT" }] }]),
        &["RouteT"],
    );
    assert!(routes_only.boot_mounts(ProtocolSide::Server).is_empty(), "an app that declares no http endpoint gets no server mounts");
    assert_eq!(mounted(routes_only.route_mounts("/")), ["RouteT"], "control: its route still mounts");
}

// ---- inbound (EXEC-INBOUND-1) ----

struct Vault(Option<&'static str>);

impl SecretSource for Vault {
    fn secret(&self, name: &str) -> Result<Option<String>, String> {
        assert_eq!(name, "SIG");
        Ok(self.0.map(str::to_string))
    }
}

struct FailingVault;

impl SecretSource for FailingVault {
    fn secret(&self, _: &str) -> Result<Option<String>, String> {
        Err("vault unreachable".to_string())
    }
}

fn hook_trait(
    name: &str,
    event: &str,
    hook: &str,
    answers: &str,
    verifies: Option<serde_json::Value>,
    transitions: Vec<serde_json::Value>,
) -> serde_json::Value {
    let mut inbound = json!({ "name": hook, "answers": answers });
    if let Some(verifies) = verifies {
        inbound["verifies"] = verifies;
    }
    json!({
        "name": name,
        "linkedEntity": "Req",
        "emits": [{ "event": answers }],
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }, { "name": "ran" }],
            "events": [{ "key": event, "inbound": inbound }],
            "transitions": transitions
        }
    })
}

fn answer_arm(event: &str, guard: serde_json::Value, answer_event: &str, to: &str, emits: Vec<serde_json::Value>) -> serde_json::Value {
    let effects: Vec<serde_json::Value> = emits.into_iter().map(|payload| json!(["emit", answer_event, payload])).collect();
    json!({ "from": "idle", "to": to, "event": event, "guard": guard, "effects": effects })
}

fn hooks_kernel() -> RuntimeKernel {
    let mode = |m: &str| json!(["=", "@payload.headers.x-mode", m]);
    let echo = hook_trait(
        "Echo",
        "ECHO",
        "echo",
        "ECHOED",
        None,
        vec![
            answer_arm("ECHO", mode("raw"), "ECHOED", "idle", vec![json!({ "status": 200, "body": "@payload.rawBody" })]),
            answer_arm(
                "ECHO",
                json!(["and", mode("parsed"), ["=", "@payload.body.n", 5]]),
                "ECHOED",
                "idle",
                vec![json!({ "status": 201, "body": "parsed-five" })],
            ),
            answer_arm("ECHO", mode("bad-status"), "ECHOED", "idle", vec![json!({ "status": 99, "body": "x" })]),
            answer_arm("ECHO", mode("bad-shape"), "ECHOED", "idle", vec![json!({ "status": 200 })]),
            answer_arm(
                "ECHO",
                mode("twice"),
                "ECHOED",
                "idle",
                vec![json!({ "status": 202, "body": "first" }), json!({ "status": 203, "body": "second" })],
            ),
        ],
    );
    let ran = |event: &str, answers: &str| {
        vec![answer_arm(event, json!(["=", 1, 1]), answers, "ran", vec![json!({ "status": 200, "body": "ok" })])]
    };
    let shown = hook_trait("Shown", "SHOW", "shown", "SHOWN", None, ran("SHOW", "SHOWN"));
    let hidden = hook_trait("Hidden", "HIDE", "hidden", "HIDDEN", None, ran("HIDE", "HIDDEN"));
    let mut twin = hook_trait("Twin", "TWIN_A", "double", "TWIN_ANSWERED", None, ran("TWIN_A", "TWIN_ANSWERED"));
    twin["stateMachine"]["events"]
        .as_array_mut()
        .expect("events list")
        .push(json!({ "key": "TWIN_B", "inbound": { "name": "double", "answers": "TWIN_ANSWERED" } }));
    twin["stateMachine"]["transitions"]
        .as_array_mut()
        .expect("transitions list")
        .extend(ran("TWIN_B", "TWIN_ANSWERED"));
    let guarded = hook_trait(
        "Guarded",
        "GUARD",
        "guarded",
        "GUARDED",
        Some(json!({ "header": "x-sig", "credential": "SIG" })),
        vec![
            answer_arm("GUARD", json!(["=", "@payload.verified", true]), "GUARDED", "ran", vec![json!({ "status": 200, "body": "verified" })]),
            answer_arm("GUARD", json!(["=", "@payload.verified", false]), "GUARDED", "ran", vec![json!({ "status": 403, "body": "forbidden" })]),
        ],
    );
    TestKernel::server().build(json!({
        "name": "Hooks",
        "inbound": ["echo", "shown", "double", "guarded", "unhandled"],
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Req", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [echo, shown, hidden, twin, guarded]
        }]
    }))
}

fn deliver(kernel: &RuntimeKernel, name: &str, headers: &[(&str, &str)], raw: &str, secrets: &dyn SecretSource) -> InboundAnswer {
    kernel.deliver_inbound(name, headers.iter().copied(), raw, secrets)
}

fn pair(answer: &InboundAnswer) -> (u16, &str) {
    (answer.status, answer.body.as_str())
}

/// EXEC-INBOUND-1: the host carries bytes; the kernel decides whether the name is served, the payload, the answer and the verification.
#[test]
fn exec_inbound_1_the_kernel_serves_declared_hooks_and_decides_the_answer_and_the_verification() {
    let kernel = hooks_kernel();
    let none = Vault(None);

    for name in ["hidden", "unhandled", "double", "nope"] {
        let answer = deliver(&kernel, name, &[], "", &none);
        assert_eq!(answer.status, 404, "`{name}` is not served");
        assert_eq!(answer.body, format!(r#"{{"error":"Unknown hook provider: {name}"}}"#));
    }
    assert_ne!(kernel.snapshot_trait("Hidden", None).state, "ran", "a hook the app does not declare runs nothing");
    assert_ne!(kernel.snapshot_trait("Twin", None).state, "ran", "a hook two traits handle runs nothing");
    assert_eq!(pair(&deliver(&kernel, "shown", &[], "", &none)), (200, "ok"), "control: a declared hook with one handler is served");
    assert_eq!(kernel.snapshot_trait("Shown", None).state, "ran");

    let raw = "  {\"n\": 5} \n";
    assert_eq!(pair(&deliver(&kernel, "echo", &[("X-Mode", "raw")], raw, &none)), (200, raw), "headers arrive lower-cased and rawBody is the exact text");
    assert_eq!(pair(&deliver(&kernel, "echo", &[("X-Mode", "parsed")], raw, &none)), (201, "parsed-five"), "body is the parsed JSON of rawBody");
    let unparsed = deliver(&kernel, "echo", &[("X-Mode", "parsed")], "not json", &none);
    assert_eq!(unparsed.status, 500, "control: a body that is not JSON is null, so the arm does not take it");
    assert!(!unparsed.body.contains("parsed-five"));
    assert_eq!(pair(&deliver(&kernel, "echo", &[], raw, &none)), (500, r#"{"error":"inbound-unanswered"}"#), "a handler that emits no answer is unanswered");

    assert_eq!(pair(&deliver(&kernel, "echo", &[("x-mode", "twice")], "", &none)), (202, "first"), "the first emit of the answers event is the answer");
    assert_eq!(
        pair(&deliver(&kernel, "echo", &[("x-mode", "bad-status")], "", &none)),
        (500, r#"{"error":"inbound-answer-invalid"}"#),
        "a status outside 100..599 is an invalid answer"
    );
    assert_eq!(pair(&deliver(&kernel, "echo", &[("x-mode", "bad-shape")], "", &none)), (500, r#"{"error":"inbound-answer-invalid"}"#), "a payload with no body text is an invalid answer");

    assert_eq!(
        pair(&deliver(&kernel, "guarded", &[("X-Sig", "s")], "", &none)),
        (401, r#"{"error":"credential SIG is not set"}"#),
        "an unset credential is 401, never no check"
    );
    assert_eq!(deliver(&kernel, "guarded", &[("X-Sig", "")], "", &Vault(Some(""))).status, 401, "an empty credential is unset");
    assert_eq!(
        pair(&deliver(&kernel, "guarded", &[("X-Sig", "s")], "", &FailingVault)),
        (500, r#"{"error":"vault unreachable"}"#),
        "a failed credential lookup is 500"
    );
    assert_ne!(kernel.snapshot_trait("Guarded", None).state, "ran", "a refused verification runs nothing");
    assert_eq!(pair(&deliver(&kernel, "guarded", &[("X-Sig", "s")], "", &Vault(Some("s")))), (200, "verified"), "a matching header verifies");
    assert_eq!(pair(&deliver(&hooks_kernel(), "guarded", &[("X-Sig", "guess")], "", &Vault(Some("s")))), (403, "forbidden"), "control: a mismatching header delivers verified=false");

    assert_eq!(kernel.inbound_only_trait("Main", Some("Echo"), "ECHO"), Some("Echo".to_string()), "a hook event is inbound-only");
    assert_eq!(kernel.inbound_only_trait("Main", Some("Echo"), "INIT"), None, "control: an ordinary event is not");
}
