// The public result and progress types below are large by design: boxing a variant or an error would
// break every caller's constructors, exhaustive matches and `Result` signatures. The production items
// carry item-local `#[expect(clippy::large_enum_variant)]` / `#[expect(clippy::result_large_err)]`
// pointing here. This file is what makes that retention safe: it pins the unboxed constructors and
// matches, the owned return of the uninstalled input, the wire bytes, and the measured layout, so
// growth fails a test instead of passing silently under an expectation.

use orbital_core_runtime::embed::wire::{PendingRequestWire, ServerProgressWire};
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress, ServerResolve};
use orbital_core_base::language_data::{install_language_data, language_data, load_language_data, LanguageData};
use orbital_core_runtime::resumable::{BlockingHost, DispatchOutcomes, IoAnswer, IoReply, IoRequest, PendingRequest, Progress, ResolveProgress};
use {orbital_core_runtime::row_query::{QueryCapabilities, RowPage}, orbital_core_base::row_set::RowSetRef};
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use orbital_core_base::schema::EntityRef;
use orbital_core_runtime::sidecar_store::{ensure_sidecar, SidecarError, SidecarFetcher};
use orbital_core_base::behavior_packages::LockedSidecar;
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::mem::size_of;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "orb-public-layout-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn v(j: &Json) -> Value {
    serde_json::from_value(j.clone()).expect("a value")
}

fn notes_schema() -> OrbitalSchema {
    let emit = json!({ "emit": { "success": "SAVED", "failure": "SAVE_FAILED" } });
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{
                "name": "Notes", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "SAVE",
                          "effects": [["persist", "create", "Note", { "id": "n1", "title": "t" }, emit]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn tickets_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "rsr",
        "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Ticket", "persistence": "persistent",
                "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "status", "type": "string" },
                    { "name": "owner", "type": "string" }
                ]
            },
            "traits": [{
                "name": "Tickets", "linkedEntity": "Ticket",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "NOOP" }]
                }
            }]
        }]
    }))
    .unwrap()
}

fn answer(store: &InMemoryEntityCache, request: &IoRequest) -> IoAnswer {
    BlockingHost {
        source: store as &dyn EntitySource,
        service: None,
        substrate: None,
    }
    .answer(request)
}

fn replies(store: &InMemoryEntityCache, requests: &[PendingRequest]) -> Vec<IoReply> {
    requests
        .iter()
        .map(|p| IoReply {
            id: p.id,
            answer: answer(store, &p.request),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// ensure_sidecar / SidecarError
// ---------------------------------------------------------------------------------------------

struct Fetch(Result<Vec<u8>, String>);

impl SidecarFetcher for Fetch {
    fn fetch(&self, _url: &str, sink: &mut dyn Write) -> Result<(), String> {
        match &self.0 {
            Ok(body) => sink.write_all(body).map_err(|e| e.to_string()),
            Err(reason) => Err(reason.clone()),
        }
    }
}

fn locked(dir: &Path, body: &[u8]) -> LockedSidecar {
    let sha256: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    LockedSidecar {
        rel_path: "dist/vectors.vec".to_string(),
        cache_path: dir.join(&sha256),
        sha256,
        bytes: body.len() as u64,
        url: "https://sidecars.example.test/vectors.vec".to_string(),
    }
}

#[test]
fn sidecar_error_variants_are_unboxed_and_match_exhaustively() {
    let fetch = SidecarError::FetchFailed {
        package: "p".into(),
        version: "1.0.0".into(),
        path: "a".into(),
        sha256: "h".into(),
        host: "example.test".into(),
        reason: "down".into(),
    };
    let mismatch = SidecarError::HashMismatch {
        package: "p".into(),
        version: "1.0.0".into(),
        path: "a".into(),
        sha256: "h".into(),
        bytes: 3,
        detail: "short".into(),
    };
    for error in [&fetch, &mismatch] {
        match error {
            SidecarError::FetchFailed { host, .. } => assert_eq!(host, "example.test"),
            SidecarError::HashMismatch { bytes, .. } => assert_eq!(*bytes, 3),
        }
    }
    assert_eq!(
        fetch.to_string(),
        "ORB_SIDECAR_FETCH_FAILED: p@1.0.0 a (sha256 h) from example.test — down"
    );
    assert_eq!(
        mismatch.to_string(),
        "ORB_SIDECAR_HASH_MISMATCH: p@1.0.0 a — expected sha256 h (3 bytes), short"
    );
}

#[test]
fn ensure_sidecar_returns_the_path_or_the_owned_unboxed_error() {
    let tmp = Tmp::new("sidecar");
    let body = b"vector bytes";

    let ok = locked(&tmp.0.join("ok"), body);
    let path: PathBuf = ensure_sidecar("p", "1.0.0", &ok, &Fetch(Ok(body.to_vec()))).unwrap();
    assert_eq!(path, ok.cache_path);
    assert_eq!(fs::read(&path).unwrap(), body);

    let bad = locked(&tmp.0.join("bad"), body);
    let err = ensure_sidecar("p", "1.0.0", &bad, &Fetch(Ok(b"other bytes".to_vec()))).unwrap_err();
    match err {
        SidecarError::HashMismatch { package, bytes, .. } => {
            assert_eq!(package, "p");
            assert_eq!(bytes, body.len() as u64);
        }
        SidecarError::FetchFailed { .. } => panic!("a wrong body is a hash mismatch"),
    }
    assert!(!bad.cache_path.exists(), "a bad download never stays in the cache");

    let down = locked(&tmp.0.join("down"), body);
    let err = ensure_sidecar(
        "p",
        "1.0.0",
        &down,
        &Fetch(Err(format!("cannot reach {}", down.url))),
    )
    .unwrap_err();
    match err {
        SidecarError::FetchFailed { host, reason, .. } => {
            assert_eq!(host, "sidecars.example.test");
            assert_eq!(reason, "cannot reach <redacted url>");
        }
        SidecarError::HashMismatch { .. } => panic!("a failed fetch is a fetch failure"),
    }
}

// ---------------------------------------------------------------------------------------------
// install_language_data / LanguageData
// ---------------------------------------------------------------------------------------------

fn install_core(root: &Path, version: &str) {
    let dir = root.join("node_modules/@almadar/core");
    fs::create_dir_all(dir.join("dist/patterns")).unwrap();
    fs::create_dir_all(dir.join("dist/language")).unwrap();
    fs::create_dir_all(dir.join("themes")).unwrap();
    fs::write(
        dir.join("package.json"),
        json!({
            "name": "@almadar/core",
            "version": version,
            "orbLanguage": {
                "compiler": ">=17.0.0",
                "patterns": "dist/patterns/patterns-registry.json",
                "componentMapping": "dist/patterns/component-mapping.json",
                "eventContracts": "dist/patterns/event-contracts.json",
                "services": "dist/patterns/services-registry.json",
                "bindingRules": "dist/language/binding-rules.json",
                "programTypes": "dist/language/program-types.json",
                "themes": "themes"
            }
        })
        .to_string(),
    )
    .unwrap();
    for (file, body) in [
        ("patterns-registry.json", r#"{"patterns":{"p":{}}}"#),
        ("component-mapping.json", r#"{"mappings":{}}"#),
        ("event-contracts.json", r#"{"contracts":{}}"#),
        ("services-registry.json", r#"{"services":{}}"#),
    ] {
        fs::write(dir.join("dist/patterns").join(file), body).unwrap();
    }
    fs::write(
        dir.join("dist/language/binding-rules.json"),
        r#"{"guard":{"allowed":["zz"]}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("dist/language/program-types.json"),
        r#"{"OrbitalValue":{"type":"object","properties":{"behavior":{"type":"string","required":true}}}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("themes/zz-theme.json"),
        r#"{"name":"zz-theme","tokens":{}}"#,
    )
    .unwrap();
}

fn load(tmp: &Tmp, version: &str) -> LanguageData {
    install_core(&tmp.0, version);
    load_language_data(&tmp.0, &tmp.0.join("store")).unwrap()
}

#[test]
fn a_language_data_collision_returns_the_uninstalled_input_not_the_installed_data() {
    let (first_dir, second_dir) = (Tmp::new("lang-first"), Tmp::new("lang-second"));
    let first = load(&first_dir, "10.1.0");
    let second = load(&second_dir, "10.2.0");
    assert_ne!(first, second);

    match install_language_data(first.clone()) {
        Ok(()) => assert_eq!(language_data().unwrap(), &first),
        Err(returned) => assert_eq!(returned, first, "a refused install hands back its own input"),
    }

    let returned: LanguageData = install_language_data(second.clone())
        .expect_err("the process-wide data is already set, so the second install collides");
    assert_eq!(returned, second, "the caller gets the data it passed in");
    assert_ne!(returned.core_version, "10.1.0");
    assert_ne!(
        language_data().unwrap(),
        &second,
        "the collision must not replace the installed data"
    );
}

// ---------------------------------------------------------------------------------------------
// Progress / ResolveProgress / ServerProgress / ServerResolve
// ---------------------------------------------------------------------------------------------

#[test]
fn progress_awaits_with_an_unboxed_continuation_and_resumes_by_value() {
    let server = EmbeddedServer::new(notes_schema()).unwrap();
    let store = InMemoryEntityCache::new();
    let mut progress: Progress<DispatchOutcomes> = server
        .kernel()
        .start_dispatch("Notes", None, "SAVE", Value::Null, None, None)
        .unwrap();
    let mut awaits = 0;
    let outcomes = loop {
        match progress {
            Progress::Done(outcomes) => break outcomes,
            Progress::Await {
                continuation,
                requests,
            } => {
                awaits += 1;
                assert!(awaits < 8, "the dispatch does not settle");
                assert!(!requests.is_empty());
                progress = server
                    .kernel()
                    .resume(continuation, replies(&store, &requests))
                    .unwrap();
            }
        }
    };
    assert!(awaits >= 1, "a persist awaits the host store");
    assert!(outcomes.iter().any(|(name, r)| name == "Notes" && r.executed));
}

fn tickets_store() -> InMemoryEntityCache {
    let store = InMemoryEntityCache::new();
    for row in [
        json!({ "id": "t1", "status": "open", "owner": "u1" }),
        json!({ "id": "t2", "status": "open", "owner": "u1" }),
    ] {
        store
            .write("Ticket", row["id"].as_str().unwrap(), &v(&row))
            .unwrap();
    }
    store
}

fn tickets_ref() -> RowSetRef {
    serde_json::from_value(json!({
        "entityType": "Ticket", "ids": ["t1", "t2"], "total": 2,
        "query": { "filter": { "kind": "compare", "field": "status", "op": "==", "value": "open" } }
    }))
    .unwrap()
}

#[test]
fn resolve_progress_awaits_with_an_unboxed_continuation_and_resumes_by_value() {
    let server = EmbeddedServer::new(tickets_schema()).unwrap();
    server
        .kernel()
        .set_query_capabilities("Ticket", QueryCapabilities::full());
    let store = tickets_store();
    let viewer = v(&json!({ "id": "u1" }));
    let mut progress: ResolveProgress = server
        .kernel()
        .start_resolve_rows(&tickets_ref(), &viewer)
        .unwrap();
    let mut awaits = 0;
    let page: RowPage = loop {
        match progress {
            ResolveProgress::Done(page) => break page,
            ResolveProgress::Await {
                continuation,
                requests,
            } => {
                awaits += 1;
                assert!(awaits < 8, "the resolve does not settle");
                progress = server
                    .kernel()
                    .resume_resolve_rows(continuation, replies(&store, &requests))
                    .unwrap();
            }
        }
    };
    assert!(awaits >= 1);
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.total, 2);
}

#[test]
fn server_progress_matches_exhaustively_and_keeps_its_wire_bytes() {
    let done = ServerProgress::Done(EventResponse::default());
    let waiting = ServerProgress::Await {
        flight: 7,
        requests: Vec::new(),
    };
    for progress in [&done, &waiting] {
        match progress {
            ServerProgress::Done(response) => assert_eq!(*response, EventResponse::default()),
            ServerProgress::Await { flight, requests } => {
                assert_eq!(*flight, 7);
                assert!(requests.is_empty());
            }
        }
    }
    assert_eq!(
        serde_json::to_value(ServerProgressWire::from(&waiting)).unwrap(),
        json!({ "status": "await", "flight": 7, "requests": [] })
    );
    assert_eq!(
        serde_json::to_value(ServerProgressWire::from(&done)).unwrap(),
        json!({ "status": "done", "response": serde_json::to_value(EventResponse::default()).unwrap() })
    );
    assert_eq!(
        serde_json::to_string(&ServerProgressWire::from(&waiting)).unwrap(),
        r#"{"status":"await","flight":7,"requests":[]}"#
    );

    let wire_done = ServerProgressWire::Done {
        response: EventResponse::default(),
    };
    let wire_wait = ServerProgressWire::Await {
        flight: 1,
        requests: Vec::<PendingRequestWire>::new(),
    };
    for wire in [&wire_done, &wire_wait] {
        match wire {
            ServerProgressWire::Done { response } => {
                assert_eq!(*response, EventResponse::default())
            }
            ServerProgressWire::Await { flight, requests } => {
                assert_eq!(*flight, 1);
                assert!(requests.is_empty());
            }
        }
    }
}

#[test]
fn a_real_server_flight_reports_the_same_status_tags_on_the_wire() {
    let server = EmbeddedServer::new(notes_schema()).unwrap();
    let store = InMemoryEntityCache::new();
    let request = EventRequest {
        event: "SAVE".to_string(),
        target_trait: Some("Notes".to_string()),
        ..Default::default()
    };
    let mut progress = server.start("Main", request).unwrap();
    let mut tags = Vec::new();
    loop {
        let wire = serde_json::to_value(ServerProgressWire::from(&progress)).unwrap();
        tags.push(wire["status"].as_str().unwrap().to_string());
        match progress {
            ServerProgress::Done(response) => {
                assert!(response.success);
                break;
            }
            ServerProgress::Await { flight, requests } => {
                assert!(tags.len() < 8, "the flight does not settle");
                progress = server.resume(flight, replies(&store, &requests)).unwrap();
            }
        }
    }
    assert_eq!(tags.first().map(String::as_str), Some("await"));
    assert_eq!(tags.last().map(String::as_str), Some("done"));
    assert_eq!(server.in_flight(), 0);
}

#[test]
fn server_resolve_stays_a_two_variant_match() {
    let server = EmbeddedServer::new(tickets_schema()).unwrap();
    server
        .kernel()
        .set_query_capabilities("Ticket", QueryCapabilities::full());
    let viewer = v(&json!({ "id": "u1" }));
    let first = server.resolve_rows(&tickets_ref(), &viewer).unwrap();
    match first {
        ServerResolve::Done(page) => assert!(page.rows.len() <= 2),
        ServerResolve::Await { flight, requests } => {
            assert!(!requests.is_empty());
            assert!(server.abandon(flight));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// EntityRef
// ---------------------------------------------------------------------------------------------

#[test]
fn entity_ref_variants_are_unboxed_and_keep_their_untagged_wire_bytes() {
    let reference: EntityRef = serde_json::from_value(json!("Alias.thing")).unwrap();
    let extends: EntityRef =
        serde_json::from_value(json!({ "extends": "Alias.thing", "name": "Mine" })).unwrap();
    let inline: EntityRef = serde_json::from_value(
        json!({ "name": "Note", "fields": [{ "name": "id", "type": "string" }] }),
    )
    .unwrap();
    for (entity, expected) in [(&reference, "ref"), (&extends, "extends"), (&inline, "inline")] {
        let seen = match entity {
            EntityRef::Reference(alias) => {
                assert_eq!(alias, "Alias.thing");
                "ref"
            }
            EntityRef::Extends(e) => {
                assert_eq!(e.reference, "Alias.thing");
                assert_eq!(e.name.as_deref(), Some("Mine"));
                "extends"
            }
            EntityRef::Inline(definition) => {
                assert_eq!(definition.name, "Note");
                "inline"
            }
        };
        assert_eq!(seen, expected);
    }
    assert_eq!(serde_json::to_value(&reference).unwrap(), json!("Alias.thing"));
    assert_eq!(
        serde_json::to_value(&extends).unwrap(),
        json!({ "extends": "Alias.thing", "name": "Mine" })
    );
    let written = serde_json::to_value(&inline).unwrap();
    assert_eq!(written["name"], "Note");
    assert_eq!(written["fields"][0]["name"], "id");
    let again: EntityRef = serde_json::from_value(written).unwrap();
    assert!(matches!(again, EntityRef::Inline(_)));
}

// ---------------------------------------------------------------------------------------------
// Layout-growth guards (native sizes; a wasm32 pointer width is not measured here)
// ---------------------------------------------------------------------------------------------

#[cfg(target_pointer_width = "64")]
#[test]
fn public_layouts_do_not_grow_past_their_measured_native_size() {
    assert_eq!(size_of::<ServerProgress>(), 440, "ServerProgress");
    assert_eq!(size_of::<ServerProgressWire>(), 440, "ServerProgressWire");
    // 704: a dispatch continuation keeps the rows the host has answered for it (`ReadCache`, one `Arc`: a row is asked
    // once) and the viewer it was started as (one `Arc`: concurrent requests keep their own `@user`). 712: plus the
    // transition its suspended step committed (one `Box`, G-CROSS-259).
    assert_eq!(size_of::<Progress<DispatchOutcomes>>(), 712, "Progress");
    // 496: a `RowQuery` carries its `nearest` selection (EXEC-FETCH-2) behind a box, 8 bytes; inline it was 56 (544).
    assert_eq!(size_of::<ResolveProgress>(), 496, "ResolveProgress");
    // 568: plus the entity's `@invariant` propositions (`Option<Vec<SExpression>>`, 24 bytes, G-ORB-384).
    assert_eq!(size_of::<EntityRef>(), 568, "EntityRef");
    assert_eq!(size_of::<SidecarError>(), 144, "SidecarError");
    assert_eq!(size_of::<LanguageData>(), 216, "LanguageData");
}
