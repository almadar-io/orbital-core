#[test]
fn a_bus_event_source_round_trips_pending_key() {
    let wire = serde_json::json!({ "trait": "Form", "pendingKey": "k-1" });
    let parsed: super::BusEventSource = serde_json::from_value(wire.clone()).expect("parses");
    assert_eq!(parsed.pending_key.as_deref(), Some("k-1"));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
    let bare = serde_json::to_value(super::BusEventSource::from_trait("Form")).unwrap();
    assert_eq!(bare, serde_json::json!({ "trait": "Form" }));
}

#[test]
fn a_mount_seed_accepts_only_lifecycle_events() {
    for e in ["INIT", "LOAD", "$MOUNT"] {
        let wire = serde_json::json!({ "trait": "T", "event": e });
        let seed: super::MountSeed =
            serde_json::from_value(wire.clone()).expect("lifecycle parses");
        assert_eq!(seed.event.as_str(), e);
        assert_eq!(serde_json::to_value(&seed).unwrap(), wire);
    }
    let bad = serde_json::json!({ "trait": "T", "event": "SAVE" });
    assert!(serde_json::from_value::<super::MountSeed>(bad).is_err());
}

#[test]
fn an_effect_failed_rejection_has_the_core_wire_shape() {
    let wire = serde_json::json!({ "code": "effect-failed", "trait": "Preview", "from": "idle", "event": "INIT", "error": "boom" });
    let parsed: super::TransitionRejection =
        serde_json::from_value(wire.clone()).expect("parses");
    assert_eq!(parsed.code, super::TransitionRejectionCode::EffectFailed);
    assert_eq!(parsed.error.as_deref(), Some("boom"));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
}

#[test]
fn control_a_rejection_without_an_error_omits_the_field() {
    let wire = serde_json::json!({ "code": "no-matching-transition", "trait": "Preview", "event": "SAVE" });
    let parsed: super::TransitionRejection =
        serde_json::from_value(wire.clone()).expect("parses");
    assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
}

fn painted(trait_name: &str, slot: &str) -> (String, Effect) {
    (
        trait_name.to_string(),
        Effect::RenderUi {
            slot: slot.to_string(),
            content: Value::Null,
        },
    )
}

fn response_of(paints: &[(String, Effect)]) -> EventResponse {
    let mut r = EventResponse::default();
    for (t, e) in paints {
        let ce = ClientEffect::from_effect(e).unwrap();
        r.client_effects_by_trait
            .get_or_insert_with(Vec::new)
            .push(ClientEffectByTrait {
                trait_name: t.clone(),
                effect: ce,
                event: None,
                from_state: None,
                callsite_payload: None,
                repaint: None,
            });
    }
    r
}

#[test]
fn drop_renders_of_removes_only_the_named_trait_slot_from_both_lists() {
    let mut r = response_of(&[
        painted("Hud", "hud"),
        painted("Hud", "side"),
        painted("Other", "hud"),
    ]);
    r.drop_renders_of(
        &[("Hud".to_string(), "hud".to_string())]
            .into_iter()
            .collect(),
    );
    let kept: Vec<_> = r
        .client_effects_by_trait
        .as_ref()
        .unwrap()
        .iter()
        .map(|e| e.trait_name.clone())
        .collect();
    assert_eq!(kept, vec!["Hud", "Other"]);
    assert_eq!(r.client_effects().len(), 2);
}

#[test]
fn control_drop_renders_of_an_empty_set_changes_nothing() {
    let mut r = response_of(&[painted("Hud", "hud")]);
    let before = r.clone();
    r.drop_renders_of(&Default::default());
    assert_eq!(r, before);
}

#[test]
fn bus_event_source_fields_are_exactly_the_wire_keys() {
    let full = BusEventSource {
        orbital: Some(String::new()),
        orbital_id: Some(String::new()),
        r#trait: Some(String::new()),
        trait_id: Some(String::new()),
        event_id: Some(String::new()),
        transition: Some(String::new()),
        tick: Some(String::new()),
        from_bridge: Some(true),
        dispatched: Some(true),
        origin_client_id: Some(String::new()),
        pending_key: Some(String::new()),
        persist_success: Some(true),
    };
    let json = serde_json::to_value(full).unwrap();
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    // `pendingKey` is a UI activation key and `persistSuccess` a live-push mark: on the wire, but not
    // `@event.source` bindings.
    let mut declared = super::BUS_EVENT_SOURCE_FIELDS.to_vec();
    declared.push("pendingKey");
    declared.push("persistSuccess");
    declared.sort_unstable();
    assert_eq!(keys, declared);
}

use super::*;
use crate::process_result::ProcessEventResult;

/// `@almadar/core` `ClientEffectByTrait.callsitePayload` (2026-09-30, G-ORB-025): an
/// embedded child's frame carries the payload it was composed with; absent stays absent.
#[test]
fn client_effect_by_trait_carries_the_optional_callsite_payload() {
    let wire = serde_json::json!({
        "traitName": "Body",
        "effect": ["render-ui", "main", { "type": "timeline" }],
        "callsitePayload": { "data": [{ "id": "e1" }] }
    });
    let parsed: ClientEffectByTrait = serde_json::from_value(wire.clone()).expect("wire shape");
    assert!(parsed.callsite_payload.is_some());
    assert_eq!(
        serde_json::to_value(&parsed).unwrap()["callsitePayload"],
        wire["callsitePayload"]
    );

    let bare = serde_json::json!({ "traitName": "Body", "effect": ["render-ui", "main", { "type": "timeline" }] });
    let parsed: ClientEffectByTrait = serde_json::from_value(bare).expect("wire shape");
    assert!(parsed.callsite_payload.is_none());
    assert!(serde_json::to_value(&parsed)
        .unwrap()
        .get("callsitePayload")
        .is_none());
}

fn denied_persist() -> ServerEffectResult {
    ServerEffectResult {
        effect: ServerEffectKind::Persist,
        action: Some("create".into()),
        entity_type: Some("Note".into()),
        name: None,
        data: None,
        success: false,
        denied: Some(true),
        error: Some("access denied".into()),
        code: None,
        target: None,
        routed: None,
    }
}

#[test]
fn a_server_denied_persist_appears_on_the_settled_response() {
    let posted = EventResponse {
        success: true,
        effect_results: Some(vec![denied_persist()]),
        ..Default::default()
    };
    let settled = EventResponse::success("idle").with_server_effect_results(&posted);
    assert_eq!(settled.effect_results, Some(vec![denied_persist()]));
}

#[test]
fn the_server_results_follow_the_local_ones() {
    let local = ServerEffectResult {
        success: true,
        denied: None,
        error: None,
        ..denied_persist()
    };
    let mut base = EventResponse::success("idle");
    base.effect_results = Some(vec![local.clone()]);
    let posted = EventResponse {
        effect_results: Some(vec![denied_persist()]),
        ..Default::default()
    };
    let settled = base.with_server_effect_results(&posted);
    assert_eq!(settled.effect_results, Some(vec![local, denied_persist()]));
}

#[test]
fn control_a_server_answer_with_no_outcomes_adds_none() {
    let settled =
        EventResponse::success("idle").with_server_effect_results(&EventResponse::default());
    assert_eq!(settled.effect_results, None);
    let empty = EventResponse {
        effect_results: Some(Vec::new()),
        ..Default::default()
    };
    assert_eq!(
        EventResponse::success("idle")
            .with_server_effect_results(&empty)
            .effect_results,
        None
    );
}

fn empty_result(new_state: &str, executed: bool) -> ProcessEventResult {
    ProcessEventResult {
        event: None,
        executed,
        previous_state: "idle".to_string(),
        new_state: new_state.to_string(),
        effects_executed: 0,
        effect_results: Vec::new(),
        entity_data: crate::shared_entity::SharedEntity::null(),
        linked_entity: "Note".to_string(),
        payload: std::sync::Arc::new(Value::Null),
        entry: None,
        fired_arm: None,
        transition_index: None,
        entity_id: "singleton".into(),
        state_entity_id: "singleton".into(),
        guards: Vec::new(),
        effect_runs: Vec::new(),
        cascade_dropped: Vec::new(),
        cascade_cut: None,
        consumed_emits: Vec::new(),
        emit_source: Default::default(),
        failure: None,
        callsite_payload: None,
        written: crate::shared_entity::SharedEntity::null(),
    }
}

#[test]
fn a_hop_names_its_fired_arm_without_entity_recording() {
    let mut fired = empty_result("editing", true);
    fired.event = Some("KEY".to_string());
    fired.fired_arm = Some(2);
    let hops = TransitionHop::of_outcomes(&[("Modes".to_string(), fired)]);
    assert_eq!(hops.len(), 1);
    assert_eq!(hops[0].fired_arm, Some(2));
    assert_eq!(hops[0].observed, None);
}

#[test]
fn control_an_unfired_step_is_no_hop() {
    let mut blocked = empty_result("idle", false);
    blocked.event = Some("KEY".to_string());
    assert!(TransitionHop::of_outcomes(&[("Modes".to_string(), blocked)]).is_empty());
}

#[test]
fn success_and_error_constructors_compile_and_default() {
    let ok = EventResponse::success("browsing");
    assert!(ok.success);
    assert!(ok.emitted_events.is_empty());

    let err = EventResponse::error("Guard failed");
    assert!(!err.success);
    assert_eq!(err.error, Some("Guard failed".to_string()));
}

#[test]
fn from_process_results_fills_states_and_transitioned() {
    let outcomes = vec![("NoteAppLayout".to_string(), empty_result("browsing", true))];
    let resp = EventResponse::from_process_results(&outcomes, "NoteAppLayout", "INIT");
    assert!(resp.success);
    assert!(resp.transitioned);
    assert_eq!(
        resp.states.get("NoteAppLayout"),
        Some(&"browsing".to_string())
    );
    assert!(resp.guard_failed.is_none());
}

#[test]
fn from_process_results_sets_guard_failed_on_seed_rejection() {
    let outcomes = vec![("NotePersistor".to_string(), empty_result("idle", false))];
    let resp = EventResponse::from_process_results(&outcomes, "NotePersistor", "CREATE_DRAFT");
    assert_eq!(
        resp.guard_failed.as_deref(),
        Some("NotePersistor.CREATE_DRAFT")
    );
}

#[test]
fn from_process_results_does_not_set_guard_failed_for_non_seed_no_op() {
    // A tolerated no-op host (embed bubble) must not poison guard_failed.
    let outcomes = vec![
        ("Child".to_string(), empty_result("open", true)),
        ("Host".to_string(), empty_result("idle", false)),
    ];
    let resp = EventResponse::from_process_results(&outcomes, "Child", "CREATE_DRAFT");
    assert!(resp.guard_failed.is_none());
}

#[test]
fn from_process_result_matches_single_outcome_of_from_process_results() {
    let result = empty_result("browsing", true);
    let single = EventResponse::from_process_result("NoteAppLayout", &result);
    assert_eq!(
        single.states.get("NoteAppLayout"),
        Some(&"browsing".to_string())
    );
    assert!(single.guard_failed.is_none());
}

#[test]
fn emitted_events_always_serialized_as_empty_array() {
    let resp = EventResponse::success("idle");
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json.get("emittedEvents"), Some(&serde_json::json!([])));
}

fn delete_contact() -> Effect {
    Effect::Persist {
        operation: orbital_core_base::effect_types::PersistOperation::Delete,
        entity_type: "Contact".to_string(),
        data: Value::Null,
        id: Some("c1".to_string()),
        emit: None,
        guard: None,
    }
}

#[test]
fn a_persist_delegated_to_the_host_reports_no_result_of_its_own() {
    let mut result = empty_result("idle", true);
    result.effect_results = vec![EffectResult::delegated(
        delete_contact(),
        orbital_core_base::effect_types::RuntimeEnvironment::Server,
    )];
    let resp = EventResponse::from_process_results(&[("Persistor".to_string(), result)], "Persistor", "DO_DELETE");
    assert!(resp.effect_results.unwrap_or_default().is_empty());
}

#[test]
fn control_a_persist_that_failed_here_still_reports_failure() {
    let mut result = empty_result("idle", true);
    result.effect_results = vec![EffectResult::failed(delete_contact(), "Contact c1 not found")];
    let resp = EventResponse::from_process_results(&[("Persistor".to_string(), result)], "Persistor", "DO_DELETE");
    let results = resp.effect_results.expect("a failed persist is reported");
    assert_eq!(results.len(), 1);
    assert!(!results[0].success);
}

#[test]
fn client_effects_from_render_ui_and_emit_split_correctly() {
    let mut result = empty_result("open", true);
    result.effect_results = vec![
        EffectResult::executed(Effect::RenderUi {
            slot: "modal".to_string(),
            content: Value::object(ValueMap::from_iter([(
                "type".to_string(),
                Value::String(("form".to_string()).into()),
            )])),
        }),
        EffectResult::executed(Effect::Emit {
            event: "DRAFT_CREATED".to_string(),
            payload: Value::Null,
        }),
    ];
    result.emit_source = BusEventSource::from_trait("NotePersistor");
    let outcomes = vec![("NotePersistor".to_string(), result)];
    let resp = EventResponse::from_process_results(&outcomes, "NotePersistor", "CREATE_DRAFT");

    let effects = resp.client_effects();
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], ClientEffect::RenderUi { .. }));

    let by_trait = resp.client_effects_by_trait.expect("by-trait");
    assert_eq!(by_trait.len(), 1);
    assert_eq!(by_trait[0].trait_name, "NotePersistor");

    assert_eq!(resp.emitted_events.len(), 1);
    assert_eq!(resp.emitted_events[0].event, "DRAFT_CREATED");
    assert_eq!(
        resp.emitted_events[0]
            .source
            .as_ref()
            .and_then(|s| s.r#trait.clone()),
        Some("NotePersistor".to_string())
    );
}

#[test]
fn bus_event_source_round_trip_uses_trait_key() {
    let source = BusEventSource::from_trait("NotePersistor");
    let json = serde_json::to_value(&source).unwrap();
    assert_eq!(json, serde_json::json!({ "trait": "NotePersistor" }));
    let back: BusEventSource = serde_json::from_value(json).unwrap();
    assert_eq!(back, source);
}

#[test]
fn a_server_effect_result_code_round_trips_every_core_value() {
    for code in ["not-found", "no-row-key", "duplicate-id", "denied", "derived-failed"] {
        let wire = serde_json::json!({ "effect": "persist", "success": false, "code": code });
        let parsed: ServerEffectResult = serde_json::from_value(wire.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
    }
    let bad = serde_json::json!({ "effect": "persist", "success": false, "code": "forbidden" });
    assert!(serde_json::from_value::<ServerEffectResult>(bad).is_err());
}

#[test]
fn server_effect_kind_is_kebab_case() {
    let result = ServerEffectResult {
        effect: ServerEffectKind::CallService,
        action: None,
        entity_type: None,
        name: None,
        data: None,
        success: true,
        denied: None,
        error: None,
        code: None,
        target: None,
        routed: None,
    };
    let json = serde_json::to_value(&result).unwrap();
    assert!(json.get("code").is_none(), "code omitted when unset");
    assert_eq!(json.get("effect"), Some(&serde_json::json!("call-service")));
    assert!(json.get("denied").is_none(), "denied omitted when not true");
}

#[test]
fn server_effect_kind_is_spelled_like_the_effect_head() {
    assert_eq!(
        serde_json::to_value(ServerEffectKind::FetchStream).unwrap(),
        serde_json::json!("fetch-stream")
    );
    assert!(serde_json::from_value::<ServerEffectKind>(serde_json::json!("swap")).is_err());
}

#[test]
fn event_request_round_trip_traits_and_entity_by_trait() {
    let json = serde_json::json!({
        "event": "CREATE_DRAFT",
        "targetTrait": "InlineButtonRender3",
        "traits": [{ "trait": "InlineButtonRender3", "from": "click" }],
        "entityByTrait": { "InlineButtonRender3": { "id": "1" } },
    });
    let req: EventRequest = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(req.event, "CREATE_DRAFT");
    assert_eq!(req.target_trait.as_deref(), Some("InlineButtonRender3"));
    assert_eq!(req.traits.as_ref().map(|t| t.len()), Some(1));
    let back = serde_json::to_value(&req).unwrap();
    assert_eq!(back.get("event"), json.get("event"));
    assert_eq!(back.get("targetTrait"), json.get("targetTrait"));
}

#[test]
fn event_request_carries_the_viewer_locale() {
    let req: EventRequest =
        serde_json::from_value(serde_json::json!({ "event": "INIT", "locale": "ar" })).unwrap();
    assert_eq!(req.locale.as_deref(), Some("ar"));
    assert_eq!(
        serde_json::to_value(&req).unwrap().get("locale"),
        Some(&serde_json::json!("ar"))
    );
}

#[test]
fn control_an_event_request_without_a_locale_serializes_none() {
    let req: EventRequest =
        serde_json::from_value(serde_json::json!({ "event": "INIT" })).unwrap();
    assert_eq!(req.locale, None);
    assert_eq!(serde_json::to_value(&req).unwrap().get("locale"), None);
}

fn dispatch_options(options: serde_json::Value) -> impl Fn(&str) -> Option<serde_json::Value> {
    move |key| options.get(key).cloned()
}

#[test]
fn a_dispatch_option_record_parses_into_the_typed_request() {
    let options = serde_json::json!({
        "event": "SAVE", "entityId": "", "targetTrait": "Form", "locale": "ar", "mount": [],
        "traits": [{ "trait": "Form", "from": "idle" }], "viewer": { "id": "u1" },
    });
    let request = super::EventRequest::from_dispatch(serde_json::json!({ "n": "x" }), dispatch_options(options)).expect("parses");
    assert_eq!(request.event, "SAVE");
    assert_eq!(request.target_trait.as_deref(), Some("Form"));
    assert_eq!(request.locale.as_deref(), Some("ar"));
    assert_eq!(request.entity_id, None, "an empty text is absent");
    assert_eq!(request.mount, None, "an empty list is absent");
    assert_eq!(request.traits.expect("traits").len(), 1);
    assert_eq!(request.user, Some(serde_json::from_value(serde_json::json!({ "id": "u1" })).unwrap()));
    assert!(request.payload.is_some());
}

#[test]
fn a_dispatch_option_record_that_is_no_event_request_is_refused_with_the_reason() {
    let missing_event = super::EventRequest::from_dispatch(serde_json::json!({}), dispatch_options(serde_json::json!({}))).unwrap_err();
    assert!(missing_event.starts_with("program/dispatch: not an event request: "), "{missing_event}");
    assert!(super::EventRequest::from_dispatch(serde_json::json!({}), dispatch_options(serde_json::json!({ "event": 3 }))).is_err());
    let bad_mount = serde_json::json!({ "event": "INIT", "mount": [{ "trait": "T", "event": "SAVE" }] });
    assert!(super::EventRequest::from_dispatch(serde_json::json!({}), dispatch_options(bad_mount)).is_err());
}
