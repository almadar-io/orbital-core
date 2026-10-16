use orbital_core_runtime::http_service::{HttpRequest, HttpResponse, SecretSource, Transport, TransportError};
use orbital_core_runtime::resumable::{answer_http, BlockingHost, CallSite, HostAnswered, HttpHost, IoAnswer, IoRequest};
use orbital_core_base::value::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Default)]
struct Secrets {
    values: Mutex<BTreeMap<(Option<String>, String), String>>,
    reads: Mutex<Vec<(Option<String>, String)>>,
    forgotten: Mutex<Vec<(Option<String>, String)>>,
    failure: Option<String>,
}

impl Secrets {
    fn put(&self, name: &str, principal: Option<&str>, value: &str) {
        self.values.lock().unwrap().insert(
            (principal.map(str::to_string), name.to_string()),
            value.to_string(),
        );
    }
}

impl SecretSource for Secrets {
    fn secret(&self, name: &str) -> Result<Option<String>, String> {
        self.secret_of(name, None)
    }

    fn secret_of(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        let identity = (principal.map(str::to_string), name.to_string());
        self.reads.lock().unwrap().push(identity.clone());
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        Ok(self.values.lock().unwrap().get(&identity).cloned())
    }

    fn forget_of(&self, name: &str, principal: Option<&str>) -> Result<(), String> {
        let identity = (principal.map(str::to_string), name.to_string());
        self.forgotten.lock().unwrap().push(identity.clone());
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        self.values.lock().unwrap().remove(&identity);
        Ok(())
    }
}

struct NoNetwork;

impl Transport for NoNetwork {
    fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
        panic!("credential host requests must not send HTTP")
    }
}

fn with_host(secrets: &Secrets, check: impl FnOnce(&HttpHost<'_>)) {
    let on_event =
        |_: Option<&CallSite>, _: Value| panic!("credential host requests emit no stream");
    let host = HttpHost {
        transport: &NoNetwork,
        secrets,
        egress: &[],
        on_event: &on_event,
        os: None,
    };
    check(&host);
}

fn status(principal: Option<&str>) -> IoRequest {
    IoRequest::CredentialStatus {
        name: "KEY".to_string(),
        principal: principal.map(str::to_string),
    }
}

fn secret(principal: Option<&str>) -> IoRequest {
    IoRequest::Secret {
        name: "KEY".to_string(),
        principal: principal.map(str::to_string),
    }
}

fn forget(principal: Option<&str>) -> IoRequest {
    IoRequest::CredentialForget {
        name: "KEY".to_string(),
        principal: principal.map(str::to_string),
    }
}

#[test]
fn app_status_requires_nonempty_presence_without_changing_raw_secret_resolution() {
    for (value, connected) in [(None, false), (Some(""), false), (Some("app-token"), true)] {
        let secrets = Secrets::default();
        if let Some(value) = value {
            secrets.put("KEY", None, value);
        }
        with_host(&secrets, |host| {
            assert_eq!(
                answer_http(Some(host), &status(None)),
                IoAnswer::CredentialStatus(Ok(connected)),
                "app value {value:?}"
            );
            assert_eq!(
                answer_http(Some(host), &secret(None)),
                IoAnswer::Secret(Ok(value.map(str::to_string))),
                "status must not rewrite the stored value"
            );
        });
        assert_eq!(
            *secrets.reads.lock().unwrap(),
            vec![(None, "KEY".to_string()), (None, "KEY".to_string())]
        );
    }
}

#[test]
fn user_status_keeps_empty_presence_and_never_falls_back_to_the_app() {
    let secrets = Secrets::default();
    secrets.put("KEY", None, "app-token");
    secrets.put("KEY", Some("u1"), "");
    secrets.put("KEY", Some("u2"), "second-user-token");
    with_host(&secrets, |host| {
        for (principal, connected) in [("u1", true), ("u2", true), ("missing-user", false)] {
            assert_eq!(
                answer_http(Some(host), &status(Some(principal))),
                IoAnswer::CredentialStatus(Ok(connected)),
                "principal {principal}"
            );
        }
        assert_eq!(
            answer_http(Some(host), &secret(Some("u1"))),
            IoAnswer::Secret(Ok(Some(String::new())))
        );
    });
    assert_eq!(
        *secrets.reads.lock().unwrap(),
        vec![
            (Some("u1".to_string()), "KEY".to_string()),
            (Some("u2".to_string()), "KEY".to_string()),
            (Some("missing-user".to_string()), "KEY".to_string()),
            (Some("u1".to_string()), "KEY".to_string()),
        ]
    );
}

#[test]
fn blocking_host_reuses_the_same_status_policy_and_preserves_scopes() {
    let secrets = Secrets::default();
    secrets.put("KEY", None, "");
    secrets.put("KEY", Some("u1"), "user-token");
    let blocking = BlockingHost {
        source: &HostAnswered,
        service: None,
        substrate: None,
    };
    with_host(&secrets, |host| {
        for (principal, connected) in [(None, false), (Some("u1"), true), (Some("u2"), false)] {
            assert_eq!(
                blocking.answer_with_http(host, &status(principal)),
                IoAnswer::CredentialStatus(Ok(connected))
            );
        }
        assert_eq!(
            blocking.answer_with_http(host, &secret(None)),
            IoAnswer::Secret(Ok(Some(String::new())))
        );
    });
}

#[test]
fn credential_storage_errors_are_not_converted_to_disconnected() {
    let secrets = Secrets {
        failure: Some("fixture credential storage failed".to_string()),
        ..Secrets::default()
    };
    with_host(&secrets, |host| {
        for principal in [None, Some("u1")] {
            assert_eq!(
                answer_http(Some(host), &status(principal)),
                IoAnswer::CredentialStatus(Err("fixture credential storage failed".to_string()))
            );
            assert_eq!(
                answer_http(Some(host), &forget(principal)),
                IoAnswer::CredentialForget(Err("fixture credential storage failed".to_string()))
            );
        }
    });
}

#[test]
fn forget_still_delegates_the_exact_requested_identity_without_fallback() {
    let secrets = Secrets::default();
    secrets.put("KEY", None, "app-token");
    secrets.put("KEY", Some("u1"), "");
    secrets.put("KEY", Some("u2"), "second-user-token");
    with_host(&secrets, |host| {
        assert_eq!(
            answer_http(Some(host), &forget(Some("u1"))),
            IoAnswer::CredentialForget(Ok(()))
        );
        assert_eq!(
            answer_http(Some(host), &status(Some("u1"))),
            IoAnswer::CredentialStatus(Ok(false))
        );
        assert_eq!(
            answer_http(Some(host), &secret(None)),
            IoAnswer::Secret(Ok(Some("app-token".to_string())))
        );
        assert_eq!(
            answer_http(Some(host), &secret(Some("u2"))),
            IoAnswer::Secret(Ok(Some("second-user-token".to_string())))
        );
        assert_eq!(
            answer_http(Some(host), &forget(None)),
            IoAnswer::CredentialForget(Ok(()))
        );
        assert_eq!(
            answer_http(Some(host), &secret(None)),
            IoAnswer::Secret(Ok(None))
        );
    });
    assert_eq!(
        *secrets.forgotten.lock().unwrap(),
        vec![
            (Some("u1".to_string()), "KEY".to_string()),
            (None, "KEY".to_string())
        ]
    );
}

#[test]
fn no_http_host_still_reports_unavailable_for_status_and_forget() {
    for principal in [None, Some("u1")] {
        assert_eq!(
            answer_http(None, &status(principal)),
            IoAnswer::CredentialStatus(Err(
                "no HTTP host is configured for this kernel".to_string()
            ))
        );
        assert_eq!(
            answer_http(None, &forget(principal)),
            IoAnswer::CredentialForget(Err(
                "no HTTP host is configured for this kernel".to_string()
            ))
        );
    }
}
