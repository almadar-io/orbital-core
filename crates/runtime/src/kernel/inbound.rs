//! Inbound deliveries (EXEC-INBOUND-1). A host carries `{name, headers, rawBody}` from
//! `/api/hooks/<name>`; the kernel owns what it means: whether the name is served, the payload the
//! handler event is delivered with, the dispatch, and the HTTP answer read off the handler's own
//! emit of its `answers` event.

use super::*;
use crate::resumable::{DispatchError, DispatchOutcomes, Progress};
use orbital_core_base::inbound::{
    answer_of, dispatch_failed, find_inbound, inbound_payload, unknown_hook, InboundAnswer, InboundRoute,
};

impl RuntimeKernel {
    /// The handler a registered program declares for `name`, or the answer for a hook it does not serve.
    pub fn inbound_route(&self, name: &str) -> Result<InboundRoute, InboundAnswer> {
        self.schemas
            .read()
            .values()
            .find_map(|schema| find_inbound(schema, name).ok())
            .ok_or_else(|| unknown_hook(name))
    }

    /// The credential a route's `verifies` names, as the host holds it: `Ok(None)` when the route verifies
    /// nothing or the host holds no value (the channel is then unchecked). A lookup that fails is the answer.
    pub fn inbound_credential(
        route: &InboundRoute,
        secrets: &dyn crate::http_service::SecretSource,
    ) -> Result<Option<String>, InboundAnswer> {
        match &route.verifies {
            None => Ok(None),
            Some(v) => secrets
                .secret(&v.credential)
                .map_err(|message| dispatch_failed(&message)),
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
    pub fn inbound_only_trait(&self, orbital: &str, trait_name: Option<&str>, event: &str) -> Option<String> {
        self.schemas
            .read()
            .values()
            .find_map(|schema| orbital_core_base::inbound::inbound_only_trait(schema, orbital, trait_name, event))
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
        let emits =
            own()
                .flat_map(|result| &result.effect_results)
                .filter_map(|run| match (&run.effect, &run.outcome) {
                    (Effect::Emit { event, payload }, outcome) if !matches!(outcome, EffectOutcome::Failed) => {
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
            None => answer,
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
            [("X-Goog-Resource-State", state), ("X-Goog-Channel-Token", token)],
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
    fn the_kernel_checks_the_sender_against_the_credential_and_an_unset_credential_is_unchecked() {
        let k = kernel(json!(["google-calendar"]));
        let ok = deliver_with(&k, "google-calendar", "exists", "s3cret", &Vault(Some("s3cret")));
        assert_eq!(ok.status, 200);
        let bad = deliver_with(&k, "google-calendar", "exists", "guess", &Vault(Some("s3cret")));
        assert_eq!((bad.status, bad.body.as_str()), (400, r#"{"error":"bad-token"}"#));
        let unchecked = deliver_with(&k, "google-calendar", "exists", "anything", &Vault(None));
        assert_eq!(unchecked.status, 200);
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
        assert_eq!(k.inbound_only_trait("Cal", None, "PUSH"), Some("Push".to_string()));
        assert_eq!(k.inbound_only_trait("Cal", Some("Push"), "INIT"), None);
        assert_eq!(k.inbound_only_trait("Other", None, "PUSH"), None);
    }
}
