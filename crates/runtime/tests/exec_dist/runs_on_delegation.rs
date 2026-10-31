// A client executor delegates exactly the `runsOn: server` effects; twin of almadar-runtime `effect-runs-on.test.ts`.

use {orbital_core_base::effect_types::{Effect, PersistOperation, RuntimeEnvironment}, orbital_core_runtime::effects::{EffectDelegate, EffectExecutor}, orbital_core_base::runs_on::{RunsOn, EFFECT_SITES}};
use orbital_core_eval::evaluator::BindingContext;
use orbital_core_base::value::Value;
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl EffectDelegate for Recorder {
    fn delegate(&self, effect: &Effect, target: RuntimeEnvironment) -> Result<(), String> {
        assert_eq!(target, RuntimeEnvironment::Server);
        self.0.lock().push(effect.operator().to_string());
        Ok(())
    }
}

fn custom(name: &str) -> Effect {
    Effect::Custom {
        name: name.into(),
        args: vec![],
        key: None,
        emit: None,
    }
}

fn run(env: RuntimeEnvironment, effects: &[Effect]) -> Vec<String> {
    let rec = Arc::new(Recorder::default());
    let exec = EffectExecutor::new(env).with_delegate(rec.clone());
    for e in effects {
        exec.execute(e, &BindingContext::new());
    }
    let out = rec.0.lock().clone();
    out
}

fn server_ops() -> Vec<Effect> {
    let mut out = vec![
        Effect::Persist {
            operation: PersistOperation::Create,
            entity_type: "T".into(),
            data: Value::Null,
            id: None,
            emit: None,
            guard: None,
        },
        Effect::Fetch {
            entity_type: "T".into(),
            id: None,
            filter: None,
            limit: None,
            offset: None,
            order_by: None,
            include: None,
            deliver: orbital_core_base::effect_types::FetchDeliver::Rows,
            total: None,
            nearest: None,
            key: None,
            emit: None,
        },
        Effect::CallService {
            service: "s".into(),
            method: "m".into(),
            args: Value::Null,
            key: None,
            emit: None,
        },
        Effect::CancelCall { key: Value::String("k".into()) },
    ];
    out.extend(
        EFFECT_SITES
            .iter()
            .filter(|(n, s)| {
                *s == RunsOn::Server
                    && !["fetch", "call-service", "cancel-call", "persist"].contains(n)
            })
            .map(|(n, _)| custom(n)),
    );
    out
}

#[test]
fn a_client_delegates_every_server_effect_including_the_ones_it_has_no_variant_for() {
    let delegated = run(RuntimeEnvironment::Client, &server_ops());
    let mut expected: Vec<String> = EFFECT_SITES
        .iter()
        .filter(|(_, s)| *s == RunsOn::Server)
        .map(|(n, _)| n.to_string())
        .collect();
    expected.sort();
    let mut got = delegated.clone();
    got.sort();
    assert_eq!(got, expected);
}

#[test]
fn a_client_runs_client_and_any_effects_itself() {
    let local = [
        Effect::Emit {
            event: "E".into(),
            payload: Value::Null,
        },
        Effect::NavigateBack,
        Effect::RenderUi {
            slot: "main".into(),
            content: Value::Null,
        },
        custom("browser/clipboard-read"),
    ];
    assert_eq!(
        run(RuntimeEnvironment::Client, &local),
        Vec::<String>::new()
    );
}

#[test]
fn a_server_never_delegates() {
    let mut all = server_ops();
    all.push(Effect::NavigateBack);
    assert_eq!(run(RuntimeEnvironment::Server, &all), Vec::<String>::new());
}

#[test]
fn without_a_delegate_a_client_skips_a_server_effect_by_name() {
    let exec = EffectExecutor::new(RuntimeEnvironment::Client);
    let result = exec.execute(&custom("random-bytes"), &BindingContext::new());
    assert!(result.is_noop(), "{result:?}");
    assert!(format!("{:?}", result.outcome).contains("random-bytes is server-only"));
}

fn host_ask_kernel(env_client: bool, effect: serde_json::Value) -> orbital_core_runtime::kernel::RuntimeKernel {
    let schema = serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Shell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Chrome", "linkedEntity": "Shell",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "SIGN_OUT", "effects": [effect] }] } }]
        }]
    });
    let t = if env_client { crate::support::TestKernel::client() } else { crate::support::TestKernel::server() };
    t.build(schema)
}

fn host_asks(kernel: &orbital_core_runtime::kernel::RuntimeKernel, payload: Value) -> (Vec<orbital_core_base::client_effect::ClientEffect>, Vec<orbital_core_runtime::EffectRunStatus>) {
    let outcomes = kernel.dispatch("Chrome", None, "SIGN_OUT", payload).unwrap();
    let response = orbital_core_runtime::effects::response::EventResponse::from_process_results(&outcomes, "Chrome", "SIGN_OUT");
    let statuses = outcomes.iter().flat_map(|(_, r)| r.effect_runs.iter().map(|run| run.status.clone())).collect();
    (response.client_effects().into_iter().cloned().collect(), statuses)
}

// EXEC-DIST-13: a client-site host-port effect is handed to the host as a host-ask.
#[test]
fn a_client_dispatch_hands_a_client_site_host_port_effect_to_its_host_as_a_host_ask() {
    use orbital_core_base::client_effect::ClientEffect;
    let sign_out = host_asks(&host_ask_kernel(true, serde_json::json!(["auth/sign-out"])), Value::Null).0;
    assert_eq!(sign_out, vec![ClientEffect::HostAsk { op: "auth/sign-out".into(), args: vec![], completion: None }]);
    let sound = host_asks(&host_ask_kernel(true, serde_json::json!(["audio/play-sound", "footstep"])), Value::Null).0;
    assert_eq!(sound, vec![ClientEffect::HostAsk { op: "audio/play-sound".into(), args: vec![Value::String("footstep".to_string().into())], completion: None }]);
}

#[test]
fn a_host_ask_carries_its_arguments_resolved_and_the_effect_ran_not_skipped() {
    use orbital_core_base::client_effect::ClientEffect;
    let kernel = host_ask_kernel(true, serde_json::json!(["audio/change-music", "@payload.track"]));
    let payload = Value::object(orbital_core_base::value::ValueMap::from_iter([("track".into(), Value::String("theme".to_string().into()))]));
    let (asks, statuses) = host_asks(&kernel, payload);
    assert_eq!(asks, vec![ClientEffect::HostAsk { op: "audio/change-music".into(), args: vec![Value::String("theme".to_string().into())], completion: None }]);
    assert_eq!(statuses, vec![orbital_core_runtime::EffectRunStatus::Executed]);
}

#[test]
fn control_a_server_kernel_asks_no_client_host_and_a_pure_effect_makes_no_host_ask() {
    assert!(host_asks(&host_ask_kernel(false, serde_json::json!(["auth/sign-out"])), Value::Null).0.is_empty(), "the server role answers the ask itself, or fails by name");
    assert!(host_asks(&host_ask_kernel(true, serde_json::json!(["set", "@entity.x", 1])), Value::Null).0.is_empty());
}

#[test]
fn a_host_ask_that_cannot_return_its_completion_fails_by_name_instead_of_losing_it() {
    let exec = EffectExecutor::new(RuntimeEnvironment::Client);
    let keyed = Effect::Custom { name: "auth/sign-out".into(), args: vec![], key: Some(Value::String("k".to_string().into())), emit: None };
    let result = exec.execute(&keyed, &BindingContext::new());
    assert!(result.is_failed(), "{result:?}");
    assert!(result.error.as_deref().is_some_and(|e| e.contains("auth/sign-out runs on the client host")), "{result:?}");
    let plain = exec.execute(&custom("auth/sign-out"), &BindingContext::new());
    assert!(plain.is_host_asked(), "control: the same ask with no key is handed to the host: {plain:?}");
}

fn sign_in(options: serde_json::Value) -> serde_json::Value {
    serde_json::json!(["auth/sign-in", { "method": "email", "email": "@payload.email", "password": "pw" }, options])
}

fn the_email() -> Value {
    Value::object(orbital_core_base::value::ValueMap::from_iter([("email".into(), Value::String("ada@example.com".to_string().into()))]))
}

#[test]
fn a_host_ask_that_declares_emit_routes_carries_its_return_address() {
    use orbital_core_base::client_effect::{ClientEffect, HostAskCompletion};
    let routes = serde_json::json!({ "key": "login-1", "emit": { "success": "SIGNED_IN", "failure": "SIGN_IN_FAILED" } });
    let (asks, statuses) = host_asks(&host_ask_kernel(true, sign_in(routes)), the_email());
    let ClientEffect::HostAsk { op, args, completion } = &asks[0] else { panic!("a host ask: {asks:?}") };
    assert_eq!(op, "auth/sign-in");
    assert_eq!(args.len(), 1, "the request is the one argument, the options are the routes: {args:?}");
    assert_eq!(
        completion,
        &Some(HostAskCompletion {
            trait_name: "Chrome".into(),
            entity_id: None,
            key: Some("login-1".into()),
            success: Some("SIGNED_IN".into()),
            failure: Some("SIGN_IN_FAILED".into()),
        })
    );
    assert_eq!(statuses, vec![orbital_core_runtime::EffectRunStatus::Executed]);
    let wire = serde_json::to_value(&asks[0]).unwrap();
    assert_eq!(wire[0], "host-ask");
    assert_eq!(wire[3], serde_json::json!({ "trait": "Chrome", "key": "login-1", "success": "SIGNED_IN", "failure": "SIGN_IN_FAILED" }));
}

#[test]
fn a_host_ask_with_one_declared_route_returns_on_that_route_only() {
    use orbital_core_base::client_effect::ClientEffect;
    let (asks, _) = host_asks(&host_ask_kernel(true, sign_in(serde_json::json!({ "emit": { "success": "SIGNED_IN" } }))), the_email());
    let ClientEffect::HostAsk { completion: Some(completion), .. } = &asks[0] else { panic!("{asks:?}") };
    assert_eq!((completion.success.as_deref(), completion.failure.as_deref(), completion.key.as_deref()), (Some("SIGNED_IN"), None, None));
}

#[test]
fn control_a_host_ask_with_no_routes_has_no_return_address() {
    use orbital_core_base::client_effect::ClientEffect;
    let (asks, _) = host_asks(&host_ask_kernel(true, serde_json::json!(["auth/sign-out"])), Value::Null);
    assert!(matches!(&asks[0], ClientEffect::HostAsk { completion: None, .. }), "{asks:?}");
}

#[test]
fn a_host_ask_cannot_declare_a_cancelled_or_streaming_route() {
    let exec = EffectExecutor::new(RuntimeEnvironment::Client);
    for emit in [
        orbital_core_base::effect_types::FetchEmit { success: Some("OK".into()), cancelled: Some("STOPPED".into()), ..Default::default() },
        orbital_core_base::effect_types::FetchEmit { success: Some("OK".into()), on_message: Some("MSG".into()), ..Default::default() },
    ] {
        let ask = Effect::Custom { name: "auth/sign-in".into(), args: vec![], key: None, emit: Some(emit) };
        let result = exec.execute(&ask, &BindingContext::new());
        assert!(result.error.as_deref().is_some_and(|e| e.contains("cannot cancel or stream")), "{result:?}");
    }
    let empty_key = Effect::Custom {
        name: "auth/sign-in".into(),
        args: vec![],
        key: Some(Value::String(String::new().into())),
        emit: Some(orbital_core_base::effect_types::FetchEmit { success: Some("OK".into()), ..Default::default() }),
    };
    assert!(exec.execute(&empty_key, &BindingContext::new()).error.as_deref().is_some_and(|e| e.contains("non-empty")));
}

#[test]
fn control_a_server_kernel_lists_no_host_ask_for_a_routed_sign_in() {
    let routes = serde_json::json!({ "emit": { "success": "SIGNED_IN", "failure": "SIGN_IN_FAILED" } });
    assert!(host_asks(&host_ask_kernel(false, sign_in(routes)), the_email()).0.is_empty());
}

fn consent(options: serde_json::Value) -> serde_json::Value {
    serde_json::json!(["auth/consent", { "authorizeUrl": "@payload.url" }, options])
}

fn the_authorize_url() -> Value {
    Value::object(orbital_core_base::value::ValueMap::from_iter([("url".into(), Value::String("https://idp.example/authorize?state=s1".to_string().into()))]))
}

fn place_consent_on_the_client() {
    let sites = std::collections::BTreeMap::from([("auth/consent".to_string(), "client".to_string())]);
    orbital_core_base::runs_on::install_operator_sites(&sites).unwrap();
}

#[test]
fn an_auth_consent_ask_hands_the_authorize_url_to_the_host_with_its_return_address() {
    use orbital_core_base::client_effect::{ClientEffect, HostAskCompletion};
    place_consent_on_the_client();
    let routes = serde_json::json!({ "key": "consent-1", "emit": { "success": "CONSENT_GRANTED", "failure": "CONSENT_FAILED" } });
    let (asks, statuses) = host_asks(&host_ask_kernel(true, consent(routes)), the_authorize_url());
    let ClientEffect::HostAsk { op, args, completion } = &asks[0] else { panic!("a host ask: {asks:?}") };
    assert_eq!(op, "auth/consent");
    assert_eq!(args.len(), 1, "the request is the one argument, the options are the routes: {args:?}");
    assert_eq!(
        completion,
        &Some(HostAskCompletion {
            trait_name: "Chrome".into(),
            entity_id: None,
            key: Some("consent-1".into()),
            success: Some("CONSENT_GRANTED".into()),
            failure: Some("CONSENT_FAILED".into()),
        })
    );
    assert_eq!(statuses, vec![orbital_core_runtime::EffectRunStatus::Executed]);
    let wire = serde_json::to_value(&asks[0]).unwrap();
    assert_eq!(wire[1], "auth/consent");
    assert_eq!(wire[2], serde_json::json!([{ "authorizeUrl": "https://idp.example/authorize?state=s1" }]));
}

#[test]
fn control_a_server_kernel_lists_no_host_ask_for_a_routed_auth_consent() {
    place_consent_on_the_client();
    let routes = serde_json::json!({ "emit": { "success": "CONSENT_GRANTED", "failure": "CONSENT_FAILED" } });
    assert!(host_asks(&host_ask_kernel(false, consent(routes)), the_authorize_url()).0.is_empty());
}
