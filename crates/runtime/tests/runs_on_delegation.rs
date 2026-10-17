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
        custom("ref"),
        custom("watch"),
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
    let result = exec.execute(&custom("swap"), &BindingContext::new());
    assert!(result.is_noop(), "{result:?}");
    assert!(format!("{:?}", result.outcome).contains("swap is server-only"));
}
