//! Inbound deliveries (EXEC-INBOUND-1). A host carries `{name, headers, rawBody}` from
//! `/api/hooks/<name>`; the kernel owns what it means: whether the name is served, the payload the
//! handler event is delivered with, the dispatch, and the HTTP answer read off the handler's own
//! emit of its `answers` event.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::*;
use crate::resumable::{DispatchError, DispatchOutcomes, Progress};
use orbital_core_base::inbound::{
    answer_of, cascade_capped, credential_unset, dispatch_failed, find_inbound, inbound_payload,
    unknown_hook, InboundAnswer, InboundRoute,
};
#[cfg(feature = "std")]
use orbital_core_base::inbound::inbound_unanswered;

/// How long [`RuntimeKernel::await_inbound_answer`] waits for a late answer between checks that a call is still open.
#[cfg(feature = "std")]
const INBOUND_OPEN_CHECK: std::time::Duration = std::time::Duration::from_millis(50);

impl RuntimeKernel {
    /// The handler a registered program declares for `name`, or the answer for a hook it does not serve.
    pub fn inbound_route(&self, name: &str) -> Result<InboundRoute, InboundAnswer> {
        self.registry.schemas
            .read()
            .values()
            .find_map(|schema| find_inbound(schema, name).ok())
            .ok_or_else(|| unknown_hook(name))
    }

    /// The credential a route's `verifies` names, as the host holds it: `Ok(None)` when the route verifies
    /// nothing. A route that verifies a credential the host holds no value for is refused with 401
    /// ([`credential_unset`]); a lookup that fails is the answer.
    pub fn inbound_credential(
        route: &InboundRoute,
        secrets: &dyn crate::http_service::SecretSource,
    ) -> Result<Option<String>, InboundAnswer> {
        let Some(v) = &route.verifies else {
            return Ok(None);
        };
        match secrets.secret(&v.credential).map_err(|message| dispatch_failed(&message))? {
            Some(value) if !value.is_empty() => Ok(Some(value)),
            _ => Err(credential_unset(&v.credential)),
        }
    }

    /// Deliver a request to its handler as a resumable dispatch: a handler that asks the host for I/O
    /// returns [`Progress::Await`], as any dispatch does. `credential` is [`Self::inbound_credential`].
    pub fn start_inbound<'a>(
        &self,
        route: &InboundRoute,
        headers: impl IntoIterator<Item = (&'a str, &'a str)>,
        raw_body: &str,
        credential: Option<&str>,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        self.start_dispatch(
            &route.trait_name,
            None,
            &route.event,
            inbound_payload(route, headers, raw_body, credential),
            None,
            None,
        )
    }

    /// [`Self::start_inbound`] run straight through, for a blocking host: the delivery's whole answer.
    pub fn deliver_inbound<'a>(
        &self,
        name: &str,
        headers: impl IntoIterator<Item = (&'a str, &'a str)>,
        raw_body: &str,
        secrets: &dyn crate::http_service::SecretSource,
    ) -> InboundAnswer {
        let route = match self.inbound_route(name) {
            Ok(route) => route,
            Err(refused) => return refused,
        };
        let credential = match Self::inbound_credential(&route, secrets) {
            Ok(credential) => credential,
            Err(failed) => return failed,
        };
        let payload = inbound_payload(&route, headers, raw_body, credential.as_deref());
        match self.dispatch(&route.trait_name, None, &route.event, payload) {
            Ok(outcomes) => Self::inbound_answer(&route, &outcomes),
            Err(error) => dispatch_failed(&error.to_string()),
        }
    }

    /// The trait that handles `event` as an inbound hook in `orbital`, when a client-facing route's request
    /// names it. The route refuses such a request (`TransitionRejectionCode::InboundOnlyEvent`): only
    /// `deliver_inbound` / `start_inbound` deliver an inbound event.
    pub fn inbound_only_trait(
        &self,
        orbital: &str,
        trait_name: Option<&str>,
        event: &str,
    ) -> Option<String> {
        self.registry.schemas.read().values().find_map(|schema| {
            orbital_core_base::inbound::inbound_only_trait(schema, orbital, trait_name, event)
        })
    }

    /// Whether a delivery's outcomes settle it: the handler trait emitted the answer event or one of its effects failed.
    /// A delivery that did neither waits for a later dispatch its keyed completions cause ([`Self::inbound_late_answer`]).
    /// A dispatch the step cap cut also settles it: the answer it owed can no longer come ([`Self::inbound_cut`]).
    pub fn inbound_answered(route: &InboundRoute, outcomes: &DispatchOutcomes) -> bool {
        Self::inbound_late_answer(route, outcomes).is_some()
            || outcomes.iter().any(|(trait_name, result)| {
                *trait_name == route.trait_name && result.failure.is_some()
            })
    }

    /// The cap answer for a dispatch of the handler trait that `CROSS_TRAIT_CASCADE_CAP` cut before it answered.
    fn inbound_cut(route: &InboundRoute, outcomes: &DispatchOutcomes) -> Option<InboundAnswer> {
        if !outcomes
            .iter()
            .any(|(trait_name, _)| *trait_name == route.trait_name)
        {
            return None;
        }
        outcomes
            .iter()
            .find_map(|(_, result)| result.cascade_cut.as_ref())
            .map(|cut| cascade_capped(cut.cap, &cut.trait_name, &cut.event))
    }

    /// The answer a later dispatch of the handler trait carries: present only when that dispatch emitted the route's
    /// answer event (a handler whose keyed I/O completes after the delivering dispatch answers then).
    pub fn inbound_late_answer(
        route: &InboundRoute,
        outcomes: &DispatchOutcomes,
    ) -> Option<InboundAnswer> {
        let emitted = outcomes.iter().filter(|(trait_name, _)| *trait_name == route.trait_name).any(|(_, result)| {
            result.effect_results.iter().any(|run| {
                matches!((&run.effect, &run.outcome), (Effect::Emit { event, .. }, outcome) if *event == route.answer && !matches!(outcome, EffectOutcome::Failed))
            })
        });
        if emitted {
            return Some(Self::inbound_answer(route, outcomes));
        }
        Self::inbound_cut(route, outcomes)
    }

    #[cfg(feature = "std")]
    /// The answer of a delivery its delivering dispatch left unanswered ([`Self::inbound_answered`] false), on a host
    /// that waits: the first late answer `late` carries while `open_calls` reports a keyed call still open (a host
    /// counts a completion open until its dispatch has run, so a late answer is sent before its call closes). With no
    /// call left and no late answer, nothing can answer any more: `inbound-unanswered`, naming the handler. When
    /// `timeout` elapses first, or `late` hangs up, the answer is [`Self::inbound_answer`] of `outcomes`.
    pub fn await_inbound_answer(
        route: &InboundRoute,
        outcomes: &DispatchOutcomes,
        late: &std::sync::mpsc::Receiver<InboundAnswer>,
        open_calls: impl Fn() -> bool,
        timeout: Option<std::time::Duration>,
    ) -> InboundAnswer {
        use std::sync::mpsc::RecvTimeoutError;
        let deadline = timeout.map(|limit| std::time::Instant::now() + limit);
        loop {
            if !open_calls() {
                return late
                    .try_recv()
                    .unwrap_or_else(|_| inbound_unanswered(&route.trait_name, &route.event));
            }
            let wait = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return Self::inbound_answer(route, outcomes);
                    }
                    left.min(INBOUND_OPEN_CHECK)
                }
                None => INBOUND_OPEN_CHECK,
            };
            match late.recv_timeout(wait) {
                Ok(answer) => return answer,
                Err(RecvTimeoutError::Disconnected) => {
                    return Self::inbound_answer(route, outcomes)
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// The answer a finished delivery produced: the first emit (one that did not fail; a kernel with no event bus reports it `NoOp`) of the route's answer event
    /// among the handler trait's outcomes. With none, the first effect failure of the handler is the
    /// answer's message; a dispatch that neither answered nor failed is `inbound-unanswered`.
    pub fn inbound_answer(route: &InboundRoute, outcomes: &DispatchOutcomes) -> InboundAnswer {
        let own = || {
            outcomes
                .iter()
                .filter(|(trait_name, _)| *trait_name == route.trait_name)
                .map(|(_, result)| result)
        };
        let emits = own()
            .flat_map(|result| &result.effect_results)
            .filter_map(|run| match (&run.effect, &run.outcome) {
                (Effect::Emit { event, payload }, outcome)
                    if !matches!(outcome, EffectOutcome::Failed) =>
                {
                    Some((event.as_str(), payload))
                }
                _ => None,
            });
        let answer = answer_of(route, emits.clone());
        if emits.clone().any(|(event, _)| event == route.answer) {
            return answer;
        }
        match own().find_map(|result| result.failure.as_deref()) {
            Some(message) => dispatch_failed(message),
            None => Self::inbound_cut(route, outcomes).unwrap_or(answer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_source::InMemoryEntityCache;
    use serde_json::json;

    fn kernel(inbound: serde_json::Value) -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "CalendarApp",
            "inbound": inbound,
            "orbitals": [{
                "name": "Cal",
                "entity": { "name": "Req", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Push",
                    "linkedEntity": "Req",
                    "emits": [{ "event": "PUSH_ANSWERED" }, { "event": "CAL_REMOTE_CHANGED" }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "PUSH", "inbound": { "name": "google-calendar", "answers": "PUSH_ANSWERED",
                            "verifies": { "header": "x-goog-channel-token", "credential": "CHANNEL_TOKEN" } } }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "PUSH",
                              "guard": ["=", "@payload.verified", false],
                              "effects": [["emit", "PUSH_ANSWERED", { "status": 400, "body": "{\"error\":\"bad-token\"}" }]] },
                            { "from": "idle", "to": "idle", "event": "PUSH",
                              "guard": ["=", "@payload.headers.x-goog-resource-state", "sync"],
                              "effects": [["emit", "PUSH_ANSWERED", { "status": 200, "body": "{\"ok\":true,\"dispatched\":false}" }]] },
                            { "from": "idle", "to": "idle", "event": "PUSH",
                              "guard": ["=", "@payload.headers.x-goog-resource-state", "exists"],
                              "effects": [
                                ["emit", "CAL_REMOTE_CHANGED", { "resourceState": "@payload.headers.x-goog-resource-state" }],
                                ["emit", "PUSH_ANSWERED", { "status": 200, "body": "{\"ok\":true,\"dispatched\":true}" }]
                              ] }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        kernel.register_schema(schema).unwrap();
        kernel
    }

    struct Vault(Option<&'static str>);

    impl crate::http_service::SecretSource for Vault {
        fn secret(&self, name: &str) -> Result<Option<String>, String> {
            assert_eq!(name, "CHANNEL_TOKEN");
            Ok(self.0.map(str::to_string))
        }
    }

    struct Failing;

    impl crate::http_service::SecretSource for Failing {
        fn secret(&self, _: &str) -> Result<Option<String>, String> {
            Err("vault unreachable".to_string())
        }
    }

    fn deliver_with(
        kernel: &RuntimeKernel,
        name: &str,
        state: &str,
        token: &str,
        secrets: &dyn crate::http_service::SecretSource,
    ) -> InboundAnswer {
        kernel.deliver_inbound(
            name,
            [
                ("X-Goog-Resource-State", state),
                ("X-Goog-Channel-Token", token),
            ],
            "",
            secrets,
        )
    }

    fn deliver(kernel: &RuntimeKernel, name: &str, state: &str, token: &str) -> InboundAnswer {
        deliver_with(kernel, name, state, token, &Vault(Some("t")))
    }

    #[test]
    fn the_handlers_own_emit_is_the_answer() {
        let k = kernel(json!(["google-calendar"]));
        let ack = deliver(&k, "google-calendar", "sync", "t");
        assert_eq!(
            (ack.status, ack.body.as_str()),
            (200, r#"{"ok":true,"dispatched":false}"#)
        );
        let dispatched = deliver(&k, "google-calendar", "exists", "t");
        assert_eq!(
            (dispatched.status, dispatched.body.as_str()),
            (200, r#"{"ok":true,"dispatched":true}"#)
        );
        let refused = deliver(&k, "google-calendar", "exists", "wrong");
        assert_eq!(
            (refused.status, refused.body.as_str()),
            (400, r#"{"error":"bad-token"}"#)
        );
    }

    #[test]
    fn control_a_name_the_app_does_not_declare_is_404_and_runs_nothing() {
        let k = kernel(json!([]));
        let answer = deliver(&k, "google-calendar", "sync", "t");
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (404, r#"{"error":"Unknown hook provider: google-calendar"}"#)
        );
        assert_eq!(
            deliver(&kernel(json!(["google-calendar"])), "slack", "sync", "t").status,
            404
        );
    }

    #[test]
    fn a_delivery_no_arm_takes_is_unanswered() {
        let k = kernel(json!(["google-calendar"]));
        let answer = deliver(&k, "google-calendar", "not_exists", "t");
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (500, r#"{"error":"inbound-unanswered"}"#)
        );
    }

    #[test]
    fn the_kernel_checks_the_sender_against_the_credential_and_an_unset_credential_is_refused() {
        let k = kernel(json!(["google-calendar"]));
        let ok = deliver_with(
            &k,
            "google-calendar",
            "exists",
            "s3cret",
            &Vault(Some("s3cret")),
        );
        assert_eq!(ok.status, 200);
        let bad = deliver_with(
            &k,
            "google-calendar",
            "exists",
            "guess",
            &Vault(Some("s3cret")),
        );
        assert_eq!(
            (bad.status, bad.body.as_str()),
            (400, r#"{"error":"bad-token"}"#)
        );
        let unset = deliver_with(&k, "google-calendar", "exists", "anything", &Vault(None));
        assert_eq!(unset.status, 401, "a route verifying a credential the host holds no value for runs nothing: {}", unset.body);
    }

    #[test]
    fn control_a_credential_lookup_that_fails_is_a_500_and_runs_nothing() {
        let k = kernel(json!(["google-calendar"]));
        let answer = deliver_with(&k, "google-calendar", "sync", "t", &Failing);
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (500, r#"{"error":"vault unreachable"}"#)
        );
    }

    #[test]
    fn an_inbound_event_is_named_only_by_the_hook_intake() {
        let k = kernel(json!(["google-calendar"]));
        assert_eq!(
            k.inbound_only_trait("Cal", Some("Push"), "PUSH"),
            Some("Push".to_string())
        );
        assert_eq!(
            k.inbound_only_trait("Cal", None, "PUSH"),
            Some("Push".to_string())
        );
        assert_eq!(k.inbound_only_trait("Cal", Some("Push"), "INIT"), None);
        assert_eq!(k.inbound_only_trait("Other", None, "PUSH"), None);
    }

    #[test]
    fn a_later_dispatch_of_the_handler_carries_a_late_answer_and_no_other_does() {
        let kernel = kernel(json!(["google-calendar"]));
        let route = kernel.inbound_route("google-calendar").unwrap();
        let answered = kernel
            .dispatch(
                &route.trait_name,
                None,
                "PUSH",
                serde_json::from_value(
                    json!({ "headers": { "x-goog-resource-state": "sync" }, "verified": true }),
                )
                .unwrap(),
            )
            .unwrap();
        let late = RuntimeKernel::inbound_late_answer(&route, &answered)
            .expect("the handler emitted the answer event");
        assert_eq!(late.status, 200);
        assert!(RuntimeKernel::inbound_answered(&route, &answered));
        let silent = kernel
            .dispatch(
                &route.trait_name,
                None,
                "PUSH",
                serde_json::from_value(
                    json!({ "headers": { "x-goog-resource-state": "gone" }, "verified": true }),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            RuntimeKernel::inbound_late_answer(&route, &silent),
            None,
            "control: no answer event, no late answer"
        );
        assert!(!RuntimeKernel::inbound_answered(&route, &silent));
        let mut elsewhere = route.clone();
        elsewhere.trait_name = "Other".to_string();
        assert_eq!(
            RuntimeKernel::inbound_late_answer(&elsewhere, &answered),
            None,
            "control: another trait's emit answers nothing"
        );
    }

    fn unanswered_delivery() -> (InboundRoute, DispatchOutcomes) {
        let kernel = kernel(json!(["google-calendar"]));
        let route = kernel.inbound_route("google-calendar").unwrap();
        let payload = serde_json::from_value(
            json!({ "headers": { "x-goog-resource-state": "gone" }, "verified": true }),
        )
        .unwrap();
        let outcomes = kernel
            .dispatch(&route.trait_name, None, "PUSH", payload)
            .unwrap();
        assert!(!RuntimeKernel::inbound_answered(&route, &outcomes));
        (route, outcomes)
    }

    fn late_answer() -> InboundAnswer {
        InboundAnswer {
            status: 200,
            body: "late".to_string(),
            stderr: None,
            headers: None,
        }
    }

    /// G-ORB-472: a delivery with no answer and no open call left is answered, instead of parking its host forever.
    #[test]
    fn a_delivery_with_no_open_call_left_answers_unanswered_naming_the_handler() {
        let (route, outcomes) = unanswered_delivery();
        let (_sender, late) = std::sync::mpsc::channel();
        let answer = RuntimeKernel::await_inbound_answer(&route, &outcomes, &late, || false, None);
        assert_eq!(answer.status, 500, "{}", answer.body);
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(
            (
                body["code"].as_str(),
                body["trait"].as_str(),
                body["event"].as_str()
            ),
            (Some("inbound-unanswered"), Some("Push"), Some("PUSH"))
        );
    }

    #[test]
    fn control_a_late_answer_sent_as_the_last_call_closed_is_still_the_answer() {
        let (route, outcomes) = unanswered_delivery();
        let (sender, late) = std::sync::mpsc::channel();
        sender.send(late_answer()).unwrap();
        assert_eq!(
            RuntimeKernel::await_inbound_answer(&route, &outcomes, &late, || false, None),
            late_answer()
        );
    }

    #[test]
    fn control_an_open_call_keeps_the_delivery_waiting_for_its_late_answer() {
        let (route, outcomes) = unanswered_delivery();
        let (sender, late) = std::sync::mpsc::channel();
        let open = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let closer = open.clone();
        let completion = std::thread::spawn(move || {
            std::thread::sleep(INBOUND_OPEN_CHECK * 4);
            sender.send(late_answer()).unwrap();
            closer.store(false, std::sync::atomic::Ordering::SeqCst);
        });
        let answer = RuntimeKernel::await_inbound_answer(
            &route,
            &outcomes,
            &late,
            || open.load(std::sync::atomic::Ordering::SeqCst),
            Some(std::time::Duration::from_secs(10)),
        );
        completion.join().unwrap();
        assert_eq!(answer, late_answer());
    }

    #[test]
    fn control_a_call_open_past_the_timeout_answers_the_delivering_dispatch() {
        let (route, outcomes) = unanswered_delivery();
        let (_sender, late) = std::sync::mpsc::channel();
        let answer = RuntimeKernel::await_inbound_answer(
            &route,
            &outcomes,
            &late,
            || true,
            Some(INBOUND_OPEN_CHECK * 2),
        );
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (500, r#"{"error":"inbound-unanswered"}"#)
        );
    }

    fn looping_kernel() -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "LoopApp",
            "inbound": ["cli-loop"],
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Run", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Loop",
                    "linkedEntity": "Run",
                    "emits": [{ "event": "STEP_NEXT" }, { "event": "LOOP_ANSWERED" }],
                    "listens": [{ "event": "STEP_NEXT", "triggers": "STEP", "source": { "kind": "trait", "trait": "Loop" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "LOOP", "inbound": { "name": "cli-loop", "answers": "LOOP_ANSWERED" } }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "LOOP",
                              "effects": [["emit", "STEP_NEXT", { "left": "@payload.body.n" }]] },
                            { "from": "idle", "to": "idle", "event": "STEP",
                              "guard": [">", "@payload.left", 0],
                              "effects": [["emit", "STEP_NEXT", { "left": ["-", "@payload.left", 1] }]] },
                            { "from": "idle", "to": "idle", "event": "STEP",
                              "guard": ["<=", "@payload.left", 0],
                              "effects": [["emit", "LOOP_ANSWERED", { "status": 200, "body": "{\"ok\":true}" }]] }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        kernel.register_schema(schema).unwrap();
        kernel
    }

    /// G-ORBITAL-CORE-039: a handler whose answer lies past `CROSS_TRAIT_CASCADE_CAP` is answered with the cut, so a
    /// waiting host (`orb <command>`) is released instead of parked forever.
    #[test]
    fn a_handler_cut_by_the_cascade_cap_answers_the_cut() {
        let k = looping_kernel();
        let steps = CROSS_TRAIT_CASCADE_CAP + 500;
        let answer =
            k.deliver_inbound("cli-loop", [], &format!(r#"{{"n":{steps}}}"#), &Vault(None));
        assert_eq!(answer.status, 500, "{}", answer.body);
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(body["code"], "cascade-cap");
        assert_eq!(body["cap"], CROSS_TRAIT_CASCADE_CAP);
        assert_eq!(body["trait"], "Loop");
        assert_eq!(body["event"], "STEP");
        let route = k.inbound_route("cli-loop").unwrap();
        let payload = serde_json::from_value(json!({ "body": { "n": steps } })).unwrap();
        let outcomes = k.dispatch("Loop", None, "LOOP", payload).unwrap();
        assert!(
            RuntimeKernel::inbound_answered(&route, &outcomes),
            "the cut settles the delivery"
        );
        assert_eq!(
            RuntimeKernel::inbound_late_answer(&route, &outcomes).map(|a| a.status),
            Some(500)
        );
    }

    #[test]
    fn control_a_handler_under_the_cascade_cap_answers_its_own_emit() {
        let k = looping_kernel();
        let answer = k.deliver_inbound("cli-loop", [], r#"{"n":10}"#, &Vault(None));
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (200, r#"{"ok":true}"#)
        );
        let route = k.inbound_route("cli-loop").unwrap();
        let payload = serde_json::from_value(json!({ "body": { "n": 10 } })).unwrap();
        let outcomes = k.dispatch("Loop", None, "LOOP", payload).unwrap();
        assert!(outcomes.iter().all(|(_, r)| r.cascade_cut.is_none()));
        assert_eq!(
            RuntimeKernel::inbound_late_answer(&route, &outcomes).map(|a| a.status),
            Some(200)
        );
    }
}
