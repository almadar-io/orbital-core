// The server role evaluates `i18n/t` against the program's declared locales and merged catalogs, as the
// viewer's locale (`EventRequest.locale`). JS twin: `almadar-runtime/test/i18n-viewer-locale.test.ts`.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::json;
use std::collections::HashMap;

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "site",
        "locales": ["en", "ar"],
        "orbitals": [{
            "name": "Home",
            "entity": { "name": "HomeView", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "HomePage", "linkedEntity": "HomeView",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                        "effects": [["render-ui", "main", { "type": "stack", "children": [
                            { "type": "typography", "content": ["i18n/t", "site:hero.title"] },
                            { "type": "typography", "content": ["i18n/t", "site:posts.count", { "n": 3 }] },
                            { "type": "typography", "content": "@locale" }
                        ] }]] }]
                }
            }]
        }]
    }))
    .unwrap()
}

fn catalogs() -> HashMap<String, HashMap<String, String>> {
    let entry = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>()
    };
    HashMap::from([
        (
            "en".to_string(),
            entry(&[
                ("site:hero.title", "Hello"),
                ("site:posts.count", "{{n}} posts"),
            ]),
        ),
        (
            "ar".to_string(),
            entry(&[
                ("site:hero.title", "مرحبا"),
                ("site:posts.count", "{{n}} منشورات"),
            ]),
        ),
    ])
}

fn server() -> EmbeddedServer {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.set_messages(vec!["en".into(), "ar".into()], catalogs());
    server
}

fn init(server: &EmbeddedServer, locale: Option<&str>) -> Result<EventResponse, String> {
    let request = EventRequest {
        event: "INIT".to_string(),
        target_trait: Some("HomePage".to_string()),
        locale: locale.map(str::to_string),
        ..Default::default()
    };
    let mut progress = server.start("Home", request)?;
    loop {
        match progress {
            ServerProgress::Done(response) => return Ok(response),
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match pending.request {
                            IoRequest::ReadRow { .. } => IoAnswer::Row(Err("no row".to_string())),
                            other => panic!("the fixture asks for {other:?}"),
                        },
                    })
                    .collect();
                progress = server.resume(flight, replies)?;
            }
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
        }
    }
}

fn rendered(response: &EventResponse) -> String {
    serde_json::to_string(&response.client_effects()[0]).unwrap()
}

#[test]
fn the_viewers_locale_picks_the_catalog_including_a_computation() {
    let page = rendered(&init(&server(), Some("ar")).unwrap());
    assert!(page.contains("مرحبا"), "{page}");
    assert!(page.contains("3 منشورات"), "{page}");
    assert!(page.contains("\"content\":\"ar\""), "{page}");
}

#[test]
fn control_a_request_without_a_locale_renders_the_first_declared() {
    let page = rendered(&init(&server(), None).unwrap());
    assert!(page.contains("\"Hello\""), "{page}");
    assert!(page.contains("\"content\":\"en\""), "{page}");
}

#[test]
fn edge_a_locale_the_program_does_not_declare_is_refused() {
    let err = init(&server(), Some("fr")).unwrap_err();
    assert!(err.contains("\"fr\""), "{err}");
}

// A message the viewer's catalog lacks answers null (`i18n/t` is a failable operator, owner ruling 2026-10-25), never a key
// painted as text (G-ORBITAL-CORE-033): the slot paints null where the message would be, and the step does not fail.
fn painted_children(response: &EventResponse) -> Vec<serde_json::Value> {
    let effect = serde_json::to_value(&response.client_effects()[0]).unwrap();
    effect[2]["children"].as_array().unwrap().clone()
}

#[test]
fn control_without_installed_messages_the_message_paints_null_not_the_key() {
    let bare = EmbeddedServer::new(schema()).unwrap();
    let response = init(&bare, Some("ar")).unwrap();
    let children = painted_children(&response);
    assert_eq!(children[0]["content"], serde_json::Value::Null, "{children:?}");
    assert_eq!(children[1]["content"], serde_json::Value::Null, "{children:?}");
    assert_eq!(children[2]["content"], "ar", "{children:?}");
    let page = serde_json::to_string(&children).unwrap();
    assert!(!page.contains("site:hero.title") && !page.contains("site:posts.count"), "{page}");
    assert!(response.rejections.as_ref().is_none_or(|r| r.is_empty()), "{:?}", response.rejections);
}

// The client role paints the same `i18n/t` locally, from the catalogs its host installs (a browser kernel has no
// program files to read them from).
fn client_init(kernel: &orbital_core_runtime::embed::EmbeddedKernel) -> EventResponse {
    kernel.dispatch("HomePage", None, "INIT", orbital_core_base::value::Value::Null).unwrap().response
}

#[test]
fn the_client_role_paints_the_catalogs_its_host_installs() {
    let kernel = orbital_core_runtime::embed::EmbeddedKernel::new(schema(), "c1").unwrap();
    kernel.set_messages(vec!["ar".into()], catalogs());
    let page = serde_json::to_string(&client_init(&kernel).client_effects()).unwrap();
    assert!(page.contains("مرحبا"), "{page}");
    assert!(page.contains("\"content\":\"ar\""), "{page}");
}

#[test]
fn control_a_client_role_without_installed_messages_paints_null_not_the_key() {
    let kernel = orbital_core_runtime::embed::EmbeddedKernel::new(schema(), "c1").unwrap();
    let response = client_init(&kernel);
    let children = painted_children(&response);
    assert_eq!(children[0]["content"], serde_json::Value::Null, "{children:?}");
    assert_eq!(children[1]["content"], serde_json::Value::Null, "{children:?}");
    assert!(response.rejections.as_ref().is_none_or(|r| r.is_empty()), "{:?}", response.rejections);
}
