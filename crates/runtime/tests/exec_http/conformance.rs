//! EXEC-HTTP, EXEC-ENDPOINT and EXEC-INBOUND conformance: the `postgres`, `browser/tab-*` and `os/tls-upgrade`
//! asks, what mounts for each endpoint, and the inbound hook intake.

use orbital_core_base::codec::{base64_decode, base64_encode};
use orbital_core_base::external_inputs::ExternalInput;
use orbital_core_base::http_wire::{CredentialSlot, HttpRefusal};
use orbital_core_base::inbound::InboundAnswer;
use orbital_core_base::os::OsOp;
use orbital_core_base::protocols::ProtocolSide;
use orbital_core_base::tab::{TabAsk, TabCommand, TabFailure, TabFailureKind, TabOp};
use orbital_core_base::tab_call::{parse_tab_call, TabCall, WatchScope};
use orbital_core_base::value::{Value, ValueMap};
use orbital_core_runtime::driver::{Driver, DriverAnswer, DriverAsk, DriverStep};
use orbital_core_runtime::http_service::SecretSource;
use orbital_core_runtime::kernel::RuntimeKernel;
use orbital_core_runtime::postgres::{parse_postgres_call, PostgresCallSpec, PostgresDriver};
use orbital_core_runtime::redis::refusal_payload;
use orbital_core_runtime::tab::{refusal_payload as tab_refusal_payload, TabDriver};
use orbital_core_runtime::tls::{TlsEvent, TlsStream};
use serde_json::json;

use crate::support::TestKernel;

fn val(j: serde_json::Value) -> Value {
    serde_json::from_value(j).expect("test value parses")
}

fn text(s: &str) -> Value {
    Value::string(s.to_string())
}

fn field<'a>(payload: &'a Value, name: &str) -> &'a Value {
    payload
        .as_object()
        .and_then(|m| m.get(name))
        .unwrap_or_else(|| panic!("no `{name}` in {payload:?}"))
}

// ---- postgres (EXEC-HTTP-5) ----

fn pg_egress() -> Vec<String> {
    vec!["postgres://db.internal:5432".to_string()]
}

fn pg_parse(sql: &str) -> Result<PostgresCallSpec, HttpRefusal> {
    parse_postgres_call(
        &val(json!("postgres://db.internal")),
        &val(json!(sql)),
        &val(json!([])),
        &val(json!({ "user": "app", "database": "main" })),
    )
}

fn pg_msg(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn pg_auth(code: i32) -> Vec<u8> {
    pg_msg(b'R', &code.to_be_bytes())
}

fn pg_trusted_startup() -> Vec<u8> {
    let mut out = pg_auth(0);
    out.extend(pg_msg(b'K', &[0, 0, 0, 1, 0, 0, 0, 2]));
    out.extend(pg_msg(b'Z', b"I"));
    out
}

fn pg_one_row_reply() -> Vec<u8> {
    let mut out = pg_msg(b'1', &[]);
    out.extend(pg_msg(b'2', &[]));
    let mut description = 1i16.to_be_bytes().to_vec();
    description.extend_from_slice(b"n\0");
    description.extend_from_slice(&0u32.to_be_bytes());
    description.extend_from_slice(&0i16.to_be_bytes());
    description.extend_from_slice(&23u32.to_be_bytes());
    description.extend_from_slice(&4i16.to_be_bytes());
    description.extend_from_slice(&(-1i32).to_be_bytes());
    description.extend_from_slice(&0i16.to_be_bytes());
    out.extend(pg_msg(b'T', &description));
    let mut row = 1i16.to_be_bytes().to_vec();
    row.extend_from_slice(&1i32.to_be_bytes());
    row.extend_from_slice(b"1");
    out.extend(pg_msg(b'D', &row));
    out.extend(pg_msg(b'C', b"SELECT 1\0"));
    out.extend(pg_msg(b'Z', b"I"));
    out
}

struct PgRun {
    ops: Vec<String>,
    step: DriverStep,
    sent: Vec<Vec<u8>>,
}

fn ask_name(ask: &DriverAsk) -> String {
    match ask {
        DriverAsk::Os { op, .. } => op.name().to_string(),
        DriverAsk::Secret { .. } => "secret".to_string(),
        DriverAsk::Random { .. } => "random".to_string(),
        DriverAsk::Clock => "clock".to_string(),
        other => format!("{other:?}"),
    }
}

fn pg_run(call: PostgresCallSpec, password: Option<&str>, replies: Vec<Vec<u8>>) -> PgRun {
    let (mut driver, mut step) = PostgresDriver::start(call, pg_egress(), None);
    let mut ops = Vec::new();
    let mut sent: Vec<Vec<u8>> = Vec::new();
    let mut replies = replies.into_iter();
    loop {
        let DriverStep::Ask(ask) = step.clone() else {
            return PgRun { ops, step, sent };
        };
        ops.push(ask_name(&ask));
        let answer = match &ask {
            DriverAsk::Secret { .. } => DriverAnswer::Secret(Ok(password.map(str::to_string))),
            DriverAsk::Os { op: OsOp::TcpConnect, .. } => DriverAnswer::Os(Ok(Value::object(ValueMap::from_iter([("sock".to_string(), Value::Number(5.0))])))),
            DriverAsk::Os { op: OsOp::Send, args, .. } => {
                let Value::String(b64) = &args[1] else { panic!("os/send carries base64 data") };
                sent.push(base64_decode(b64).expect("os/send data is base64"));
                DriverAnswer::Os(Ok(Value::Null))
            }
            DriverAsk::Os { op: OsOp::Recv, .. } => {
                let Some(reply) = replies.next() else {
                    return PgRun { ops, step, sent };
                };
                DriverAnswer::Os(Ok(Value::object(ValueMap::from_iter([
                    ("data".to_string(), Value::string(base64_encode(&reply))),
                    ("eof".to_string(), Value::Bool(false)),
                ]))))
            }
            DriverAsk::Os { op: OsOp::Close, .. } => DriverAnswer::Os(Ok(Value::object(ValueMap::default()))),
            DriverAsk::Clock => DriverAnswer::Clock(Ok(1_760_000_000_000)),
            DriverAsk::Random { bytes } => DriverAnswer::Random(Ok(vec![7; *bytes as usize])),
            other => panic!("unexpected ask {other:?}"),
        };
        step = driver.resume(answer);
    }
}

fn done_ok(step: &DriverStep) -> &Value {
    match step {
        DriverStep::Done(Ok(v)) => v,
        other => panic!("expected success, got {other:?}"),
    }
}

fn done_err(step: &DriverStep) -> &Value {
    match step {
        DriverStep::Done(Err(v)) => v,
        other => panic!("expected failure, got {other:?}"),
    }
}

/// EXEC-HTTP-5: a `postgres` call is a journaled sequence of `os/*` asks, and the read-only guard runs before anything connects.
#[test]
fn exec_http_5_postgres_refuses_non_select_before_any_ask_then_runs_connect_startup_query_terminate_close() {
    for sql in [
        "INSERT INTO t VALUES (1)",
        "UPDATE t SET a = 1",
        "DELETE FROM t",
        "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
        "SELECT * INTO copy FROM t",
        "SELECT a FROM t FOR UPDATE",
        "SELECT 1; SELECT 2",
    ] {
        let refusal = pg_parse(sql).expect_err(&format!("`{sql}` must be refused before any ask"));
        let payload = refusal_payload(&refusal, &None);
        assert_eq!(field(&payload, "kind"), &text("refused"), "refused statement: {sql}");
        let message = match field(&payload, "error") {
            Value::String(message) => message.to_string(),
            other => panic!("error is text, got {other:?}"),
        };
        assert!(message.starts_with("Read-only violation"), "`{sql}` -> {message}");
    }
    for sql in [
        "SELECT 1",
        "WITH x AS (SELECT 1 AS n) SELECT n FROM x",
        "SELECT 'delete into' /* INSERT */ -- UPDATE",
    ] {
        assert!(pg_parse(sql).is_ok(), "control: `{sql}` is exactly one SELECT after blanking comments and quoted text");
    }

    let call = pg_parse("SELECT 1").expect("a SELECT parses");
    let run = pg_run(call.clone(), None, vec![pg_trusted_startup(), pg_one_row_reply()]);
    assert_eq!(
        run.ops,
        ["os/tcp-connect", "os/send", "os/recv", "os/send", "os/recv", "os/send", "os/close"],
        "connect, startup, query batch, terminate, close"
    );
    assert_eq!(run.sent.len(), 3);
    assert_eq!(run.sent[2], pg_msg(b'X', &[]), "the call ends with Terminate");
    let result = done_ok(&run.step);
    assert_eq!(field(result, "rowCount"), &Value::Number(1.0));
    assert!(result.get_path("columns").is_some() && result.get_path("rows").is_some());

    let mut named = call.clone();
    named.auth = Some(CredentialSlot::from("PG_PASSWORD"));
    let with_auth = pg_run(named, Some("pw"), vec![pg_trusted_startup(), pg_one_row_reply()]);
    assert_eq!(with_auth.ops[0], "secret", "an `auth` call asks for the secret before it connects");
    assert_eq!(with_auth.ops[1], "os/tcp-connect");
    assert_eq!(run.ops[0], "os/tcp-connect", "control: with no `auth` the first ask is the connect");

    let mut tls = call.clone();
    tls.tls = true;
    let declined = pg_run(tls.clone(), None, vec![b"N".to_vec()]);
    assert_eq!(declined.sent[0], vec![0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f], "with tls an SSL request is sent first");
    assert_eq!(field(done_err(&declined.step), "kind"), &text("unsupported"), "a declined upgrade is unsupported");
    let upgraded = pg_run(tls, None, vec![b"S".to_vec()]);
    assert!(upgraded.ops.contains(&"random".to_string()) && upgraded.ops.contains(&"clock".to_string()), "an accepted upgrade seeds from the journaled random and clock asks: {:?}", upgraded.ops);
    assert_ne!(run.sent[0][..4], [0, 0, 0, 8], "control: without tls no SSL request is sent");

    let unnamed = pg_run(call, None, vec![pg_auth(3)]);
    assert_eq!(field(done_err(&unnamed.step), "kind"), &text("unconfigured"), "a password demanded by a call that names no auth is unconfigured");
}

// ---- browser/tab (EXEC-HTTP-6) ----

fn tab_inputs() -> Vec<ExternalInput> {
    vec![ExternalInput {
        orbital: "Feed".to_string(),
        trait_name: "Watch".to_string(),
        event: "ITEM_SEEN".to_string(),
        payload_schema: Vec::new(),
        description: None,
        synonyms: None,
        tier: None,
    }]
}

fn tab_call(op: TabOp, request: serde_json::Value) -> Result<TabCall, String> {
    parse_tab_call(op, &val(request), None, Some("k"))
}

fn tab_run(call: TabCall, answers: Vec<Result<Value, TabFailure>>) -> (Vec<TabAsk>, DriverStep) {
    let (mut driver, mut step) = TabDriver::start(call);
    let mut asks = Vec::new();
    let mut answers = answers.into_iter();
    loop {
        match step {
            DriverStep::Ask(DriverAsk::Tab { ask }) => {
                asks.push(ask);
                let answer = answers.next().expect("the script has an answer for every ask");
                step = driver.resume(DriverAnswer::Tab(answer));
            }
            other => return (asks, other),
        }
    }
}

fn not_ready() -> TabFailure {
    TabFailure::new(TabFailureKind::NotReady, "Tab 4 has no page script")
}

/// EXEC-HTTP-6: a `browser/tab-*` call validates in the kernel (nothing asked on a bad request), then runs as raw tab asks it shapes the result of.
#[test]
fn exec_http_6_tab_calls_are_validated_by_the_kernel_and_run_as_ordered_raw_asks() {
    let refused: Vec<(TabOp, serde_json::Value)> = vec![
        (TabOp::List, json!({ "bogus": 1 })),
        (TabOp::List, json!({ "match": "not a pattern" })),
        (TabOp::Open, json!({ "url": "/relative" })),
        (TabOp::Open, json!({ "url": "https://other.test/", "match": "https://example.com/*" })),
        (TabOp::Close, json!({ "tab": "four" })),
        (TabOp::Apply, json!({ "tab": 1, "elementId": "e", "treatment": "explode" })),
        (TabOp::Act, json!({ "tab": 1, "elementId": "e", "action": "tap" })),
        (TabOp::Query, json!({ "tab": 1, "selector": ".i", "fields": { "title": 5 } })),
        (TabOp::Capture, json!({ "tab": 1, "quality": 50 })),
    ];
    for (op, request) in refused {
        let message = tab_call(op, request.clone()).expect_err(&format!("{} {request} must be refused", op.name()));
        let payload = tab_refusal_payload(&message, Some("k"));
        assert_eq!(field(&payload, "kind"), &text("invalid"), "{} {request}", op.name());
        assert_eq!(field(&payload, "key"), &text("k"));
    }
    let accepted: Vec<(TabOp, serde_json::Value)> = vec![
        (TabOp::List, json!({ "match": "https://example.com/*" })),
        (TabOp::Open, json!({ "url": "https://example.com/a", "match": "https://example.com/*" })),
        (TabOp::Close, json!({ "tab": 4 })),
        (TabOp::Apply, json!({ "tab": 1, "elementId": "e", "treatment": "hide" })),
        (TabOp::Act, json!({ "tab": 1, "elementId": "e", "action": "click" })),
        (TabOp::Query, json!({ "tab": 1, "selector": ".i", "fields": { "title": { "selector": ".t" } } })),
        (TabOp::Capture, json!({ "tab": 1, "format": "jpeg", "quality": 50 })),
    ];
    for (op, request) in accepted {
        assert!(tab_call(op, request.clone()).is_ok(), "control: {} {request} is a valid request", op.name());
    }

    let inputs = tab_inputs();
    let scope = WatchScope { caller_orbital: Some("Feed"), caller_trait: "Watch", inputs: &inputs };
    let watch = |event: &str, selector: &str| {
        parse_tab_call(
            TabOp::Watch,
            &val(json!({ "match": "https://example.com/*", "selector": selector, "fields": {}, "event": event })),
            Some(&scope),
            None,
        )
    };
    assert!(watch("NOT_DECLARED", ".item").is_err(), "a watch naming no declared external input is invalid");
    let id_of = |selector: &str| {
        let call = watch("ITEM_SEEN", selector).expect("control: a declared input is accepted");
        match tab_run(call, vec![Ok(Value::Null)]).0.remove(0) {
            TabAsk::Watch { watch_id, .. } => watch_id,
            other => panic!("expected a watch ask, got {other:?}"),
        }
    };
    assert_eq!(id_of(".item"), id_of(".item"), "the watch id is stable across restarts");
    assert_ne!(id_of(".item"), id_of(".other"), "control: a different selector is a different watch");

    let (asks, step) = tab_run(
        tab_call(TabOp::Capture, json!({ "tab": 5, "format": "jpeg", "quality": 70 })).expect("capture parses"),
        vec![Ok(text("data:image/jpeg;base64,/9j/4AAQ"))],
    );
    assert_eq!(asks.len(), 1);
    let captured = done_ok(&step);
    assert_eq!(field(captured, "tab"), &Value::Number(5.0));
    assert_eq!(field(captured, "format"), &text("jpeg"));
    assert_eq!(field(captured, "image"), &text("/9j/4AAQ"), "the image is base64 without its data-url prefix");
    assert_eq!(field(captured, "key"), &text("k"));

    let waiting = tab_call(TabOp::Query, json!({ "tab": 4, "selector": ".i", "fields": {}, "waitMs": 1000 })).expect("query parses");
    let ready = val(json!({ "ready": true, "waitedMs": 250 }));
    let (asks, step) = tab_run(waiting, vec![Err(not_ready()), Ok(ready), Ok(val(json!([])))]);
    assert_eq!(asks.len(), 3, "not-ready with a wait budget: ask, await readiness, ask again");
    assert_eq!(asks[1], TabAsk::AwaitReady { tab: 4, timeout_ms: 1000 });
    assert!(matches!(&asks[2], TabAsk::Command { command: TabCommand::Query { wait_ms: Some(750), .. }, .. }), "the second ask carries the remaining budget");
    assert!(matches!(step, DriverStep::Done(Ok(_))));
    let impatient = tab_call(TabOp::Query, json!({ "tab": 4, "selector": ".i", "fields": {} })).expect("query parses");
    let (asks, step) = tab_run(impatient, vec![Err(not_ready())]);
    assert_eq!(asks.len(), 1, "control: with no wait budget a not-ready answer fails after one ask");
    assert_eq!(field(done_err(&step), "kind"), &text("not-ready"));

    let many = tab_call(
        TabOp::ApplyMany,
        json!({ "items": [
            { "tab": 2, "elementId": "a", "treatment": "hide" },
            { "tab": 1, "elementId": "b", "treatment": "hide" },
            { "tab": 2, "elementId": "c", "treatment": "hide" }
        ] }),
    )
    .expect("apply-many parses");
    let closed = TabFailure::new(TabFailureKind::Failed, "No tab with id: 2");
    let (asks, step) = tab_run(many, vec![Err(closed), Ok(val(json!({ "applied": ["b"], "missing": [] })))]);
    assert!(matches!(&asks[0], TabAsk::Command { tab: 2, command: TabCommand::ApplyMany { items } } if items.len() == 2), "one batch per tab, first-appearance order");
    assert!(matches!(&asks[1], TabAsk::Command { tab: 1, .. }));
    let batch = done_ok(&step);
    assert_eq!(field(batch, "applied"), &val(json!([{ "tab": 1, "elementId": "b" }])));
    assert_eq!(field(batch, "missing"), &val(json!([{ "tab": 2, "elementId": "a" }, { "tab": 2, "elementId": "c" }])), "a failed tab lists its items as missing");

    for kind in [TabFailureKind::Unavailable, TabFailureKind::NotReady, TabFailureKind::NotFound, TabFailureKind::Failed] {
        let (_, step) = tab_run(tab_call(TabOp::Close, json!({ "tab": 9 })).expect("close parses"), vec![Err(TabFailure::new(kind, "host said no"))]);
        let failure = done_err(&step);
        assert_eq!(field(failure, "kind"), &text(kind.as_str()));
        assert_eq!(field(failure, "key"), &text("k"), "a failure carries the key");
    }
}

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
#[ignore = "G-ORBITAL-CORE-073: the kernel's OsOp table has no `os/tls-upgrade`; only the postgres driver upgrades a socket internally (TlsStream::upgrade)"]
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
