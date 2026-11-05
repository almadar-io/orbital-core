//! EXEC-DIST-15: `program/self { mounted: true }` answers the session's mounted orbital as a tree.

use orbital_core_base::value::Value;
use orbital_core_runtime::effects::{LifecycleEvent, MountSeed};
use orbital_core_runtime::kernel::RuntimeKernel;

fn render(children: serde_json::Value) -> serde_json::Value {
    serde_json::json!(["render-ui", "main", { "type": "stack", "children": children }])
}

fn program() -> serde_json::Value {
    let probe = serde_json::json!(["program/self", { "mounted": true, "emit": { "success": "KNOWN", "failure": "UNKNOWN" } }]);
    let machine = |name: &str, states: &[&str], transitions: serde_json::Value| {
        let states: Vec<serde_json::Value> = states
            .iter()
            .enumerate()
            .map(|(i, s)| serde_json::json!({ "name": s, "isInitial": i == 0 }))
            .collect();
        serde_json::json!({ "name": name, "linkedEntity": "Shell", "stateMachine": { "states": states, "transitions": transitions } })
    };
    let mut viewer = machine(
        "Viewer",
        &["idle"],
        serde_json::json!([{ "from": "idle", "to": "idle", "event": "SEEN" }]),
    );
    viewer["listens"] = serde_json::json!([{ "event": "KNOWN", "triggers": "SEEN", "source": { "kind": "trait", "trait": "Probe" } }]);
    let mut relay = machine(
        "Relay",
        &["idle"],
        serde_json::json!([{ "from": "idle", "to": "idle", "event": "RELAYED" }]),
    );
    relay["listens"] = serde_json::json!([{ "event": "SEEN_AGAIN", "triggers": "RELAYED", "source": { "kind": "trait", "trait": "Viewer" } }]);
    let client = |mut t: serde_json::Value| {
        t["dispatch"] = serde_json::json!("client");
        t
    };
    serde_json::json!({
        "name": "App",
        "typeContractVersion": 1,
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Shell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                machine("Frame", &["idle"], serde_json::json!([
                    { "from": "idle", "to": "idle", "event": "INIT", "effects": [render(serde_json::json!(["@trait.Header", "@trait.Board"]))] }
                ])),
                client(machine("Header", &["closed", "open"], serde_json::json!([
                    { "from": "closed", "to": "closed", "event": "INIT", "effects": [render(serde_json::json!([]))] },
                    { "from": "closed", "to": "open", "event": "OPEN", "effects": [render(serde_json::json!(["@trait.Theme"])), ["emit", "OPENED", {}]] }
                ]))),
                machine("Theme", &["idle"], serde_json::json!([
                    { "from": "idle", "to": "idle", "event": "INIT", "effects": [render(serde_json::json!([]))] }
                ])),
                machine("Board", &["idle"], serde_json::json!([
                    { "from": "idle", "to": "idle", "event": "INIT", "effects": [render(serde_json::json!(["@trait.Theme"]))] }
                ])),
                machine("Fetcher", &["idle", "loading"], serde_json::json!([
                    { "from": "idle", "to": "loading", "event": "LOAD_IT", "guard": [">", 1, 0] }
                ])),
                machine("Probe", &["idle"], serde_json::json!([
                    { "from": "idle", "to": "idle", "event": "ASK", "effects": [probe] }
                ])),
                viewer,
                relay
            ],
            "pages": [
                { "name": "Home", "path": "/", "traits": [{ "ref": "Frame" }, { "ref": "Fetcher" }, { "ref": "Probe" }, { "ref": "Viewer" }, { "ref": "Relay" }] },
                { "name": "Other", "path": "/other", "traits": [{ "ref": "Probe" }] }
            ]
        }]
    })
}

fn seed(t: &str) -> MountSeed {
    MountSeed {
        r#trait: t.to_string(),
        event: LifecycleEvent::Init,
    }
}

fn ask(kernel: &RuntimeKernel) -> Result<Value, String> {
    let outcomes = kernel.dispatch("Probe", None, "ASK", Value::Null).unwrap();
    let result = outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .find(|r| r.effect.operator() == "program/self")
        .expect("program/self ran")
        .clone();
    match result.error {
        Some(error) => Err(error),
        None => Ok(result.return_value.expect("an answer")),
    }
}

type Row = (String, Option<String>, i64, Vec<String>, bool);

fn rows(tree: &Value) -> Vec<Row> {
    let json: serde_json::Value = serde_json::to_value(tree).unwrap();
    json["traits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["trait"].as_str().unwrap().to_string(),
                t["parent"].as_str().map(str::to_string),
                t["depth"].as_i64().unwrap(),
                t["embeddedIn"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_string())
                    .collect(),
                t["renders"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn mounted(kernel: &RuntimeKernel, route: &str) -> Value {
    kernel
        .dispatch_route_mount(Some(route), &[seed("Frame")], None, Value::Null)
        .unwrap();
    ask(kernel).expect("a mounted tree")
}

fn row(name: &str, parent: Option<&str>, depth: i64, embedded_in: &[&str], renders: bool) -> Row {
    (
        name.to_string(),
        parent.map(str::to_string),
        depth,
        embedded_in.iter().map(|s| s.to_string()).collect(),
        renders,
    )
}

#[test]
fn the_mounted_tree_lists_the_pages_traits_in_declared_order_each_followed_by_what_it_embeds() {
    let t = crate::support::TestKernel::server();
    let kernel = t.build(program());
    let tree = mounted(&kernel, "/");
    let json: serde_json::Value = serde_json::to_value(&tree).unwrap();
    assert_eq!(json["page"], "Home");
    assert_eq!(
        rows(&tree),
        vec![
            row("Frame", None, 0, &[], true),
            row("Header", Some("Frame"), 1, &["idle"], true),
            row("Theme", Some("Header"), 2, &["open"], true),
            row("Board", Some("Frame"), 1, &["idle"], true),
            row("Fetcher", None, 0, &[], false),
            row("Probe", None, 0, &[], false),
            row("Viewer", None, 0, &[], false),
            row("Relay", None, 0, &[], false),
        ]
    );
    let observers: Vec<&str> = json["traits"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["observer"] == true)
        .map(|t| t["trait"].as_str().unwrap())
        .collect();
    assert_eq!(observers, vec!["Probe", "Viewer", "Relay"], "the asker, the trait that listens to it and the trait that listens to that one; control: Frame, Fetcher and the rest are not");
    let fetcher = &json["traits"][4];
    assert_eq!(fetcher["states"].as_array().unwrap().len(), 2);
    assert_eq!(
        fetcher["transitions"][0]["from"],
        serde_json::json!(["idle"])
    );
    assert_eq!(fetcher["transitions"][0]["guarded"], true);
    assert_eq!(fetcher["orbital"], "Main");
    assert_eq!(fetcher["stepsIn"], "server");
    assert_eq!(
        json["traits"][0]["instances"],
        serde_json::json!([{ "entityId": "singleton", "state": "idle" }]),
        "Frame ran its INIT"
    );
    assert_eq!(
        fetcher["instances"],
        serde_json::json!([]),
        "control: a trait that never ran has no instance"
    );
    assert_eq!(
        json["traits"][1]["stepsIn"], "browser",
        "Header is [client]"
    );
    assert_eq!(
        json["traits"][1]["transitions"][1]["emits"],
        serde_json::json!(["OPENED"])
    );
    assert_eq!(
        json["traits"][1]["transitions"][0]["emits"],
        serde_json::json!([]),
        "control: a transition that emits nothing"
    );
}

#[test]
fn control_another_route_answers_that_page_and_no_route_mount_answers_nothing() {
    let t = crate::support::TestKernel::server();
    let kernel = t.build(program());
    let error = ask(&kernel).expect_err("no page opened");
    assert!(error.contains("this session has opened no page"), "{error}");
    let tree = mounted(&kernel, "/other");
    assert_eq!(rows(&tree), vec![row("Probe", None, 0, &[], false)]);
}

#[test]
fn a_client_kernel_answers_from_its_own_session_page_and_asks_no_server() {
    let t = crate::support::TestKernel::client();
    let kernel = t.build(program());
    let tree = mounted(&kernel, "/");
    assert_eq!(rows(&tree).len(), 8);
    let outcomes = kernel.dispatch("Probe", None, "ASK", Value::Null).unwrap();
    let response = orbital_core_runtime::effects::response::EventResponse::from_process_results(
        &outcomes, "Probe", "ASK",
    );
    assert!(
        response.client_effects().is_empty(),
        "{:?}",
        response.client_effects()
    );
}

#[test]
fn a_browser_embed_route_mount_records_its_page_for_the_mounted_tree() {
    let schema: orbital_core_base::schema::types::OrbitalSchema =
        serde_json::from_value(program()).unwrap();
    let embedded = orbital_core_runtime::embed::EmbeddedKernel::new(schema, "c1").unwrap();
    embedded
        .dispatch_route_mount(Some("/"), &[seed("Frame")], None, Value::Null)
        .unwrap();
    let tree = ask(embedded.kernel()).expect("the page the browser opened");
    assert_eq!(
        rows(&tree).iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        vec!["Frame", "Header", "Theme", "Board", "Fetcher", "Probe", "Viewer", "Relay"]
    );
}

#[test]
fn control_a_browser_embed_mount_with_no_route_records_no_page() {
    let schema: orbital_core_base::schema::types::OrbitalSchema =
        serde_json::from_value(program()).unwrap();
    let embedded = orbital_core_runtime::embed::EmbeddedKernel::new(schema, "c1").unwrap();
    embedded
        .dispatch_route_mount(None, &[seed("Frame")], None, Value::Null)
        .unwrap();
    let error = ask(embedded.kernel()).expect_err("no route was opened");
    assert!(error.contains("this session has opened no page"), "{error}");
}
