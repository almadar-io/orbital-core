//! The kernel half of credential administration (`orbital_core_base::credential_admin` owns the pure rules): it asks the
//! host for the raw store and mode-flag facts, in a fixed order so a replay sees the same asks, and answers the
//! program with the masked, gated, declared-name-checked result. The host decides nothing.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::HashMap;

use orbital_core_base::credential_admin::{
    self as admin, Catalog, CredentialAdminOp, ListFacts, Refusal, StoreView, StoredEntry,
};
use orbital_core_base::value::{Value, ValueMap};
use serde::{Deserialize, Serialize};

use crate::effects::executor::ServiceCaller;
use crate::resumable::IoPort;

/// One stored credential row as the store reports it: its service and credential name, never its value.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredName {
    pub service: String,
    pub name: String,
}

/// The credential store's state: whether it can encrypt (a master key is present) and the rows it holds.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreListing {
    pub enabled: bool,
    pub entries: Vec<StoredName>,
}

/// What `credential-status` reports of one credential: whether the host holds a value, and the non-secret account label
/// stored beside it (the email it was signed in as), never the value.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialStanding {
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl CredentialStanding {
    /// A standing with no label.
    pub fn of(connected: bool) -> Self {
        Self { connected, label: None }
    }
}

/// What a rotation did to the stored rows.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RotateCounts {
    #[cfg_attr(test, ts(type = "number"))]
    pub rotated: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub already_current: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub unreadable: u64,
}

/// The asks a credential effect makes: the kernel's journaled port in a dispatch, a scripted host in a test.
pub trait AdminAsks {
    /// A credential by name, app-wide or for `principal`, as the host resolves it.
    fn secret(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String>;
    fn store_list(&self) -> Result<Option<StoreListing>, String>;
    fn store_read(&self, name: &str) -> Result<Option<String>, String>;
    fn store_write(&self, service: &str, name: &str, value: &str) -> Result<(), String>;
    fn store_remove(&self, name: &str) -> Result<bool, String>;
    fn store_rotate(&self) -> Result<RotateCounts, String>;
    fn mode_read(&self) -> Result<Option<String>, String>;
    fn mode_write(&self, mode: &str) -> Result<String, String>;
}

impl AdminAsks for IoPort {
    fn secret(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        IoPort::secret(self, name, principal)
    }
    fn store_list(&self) -> Result<Option<StoreListing>, String> {
        self.credential_store_list()
    }
    fn store_read(&self, name: &str) -> Result<Option<String>, String> {
        self.credential_store_read(name)
    }
    fn store_write(&self, service: &str, name: &str, value: &str) -> Result<(), String> {
        self.credential_store_write(service, name, value)
    }
    fn store_remove(&self, name: &str) -> Result<bool, String> {
        self.credential_store_remove(name)
    }
    fn store_rotate(&self) -> Result<RotateCounts, String> {
        self.credential_store_rotate()
    }
    fn mode_read(&self) -> Result<Option<String>, String> {
        self.credential_mode_read()
    }
    fn mode_write(&self, mode: &str) -> Result<String, String> {
        self.credential_mode_write(mode)
    }
}

fn host(message: String) -> Refusal {
    Refusal::plain(message)
}

fn env(asks: &dyn AdminAsks, name: &str) -> Result<Option<String>, Refusal> {
    Ok(asks.secret(name, None).map_err(host)?.filter(|value| !value.is_empty()))
}

fn require_store(asks: &dyn AdminAsks, suffix: &str) -> Result<StoreListing, Refusal> {
    asks.store_list()
        .map_err(host)?
        .ok_or_else(|| Refusal::plain(format!("{}{suffix}", admin::NO_STORE)))
}

fn store_view(asks: &dyn AdminAsks) -> Result<Option<StoreView>, Refusal> {
    let Some(listing) = asks.store_list().map_err(host)? else {
        return Ok(None);
    };
    let mut entries = Vec::new();
    if listing.enabled {
        for row in listing.entries {
            if let Some(value) = asks.store_read(&row.name).map_err(host)? {
                entries.push(StoredEntry { service: row.service, env_var: row.name, last4: admin::last4(&value) });
            }
        }
    }
    Ok(Some(StoreView { enabled: listing.enabled, entries }))
}

fn env_map(asks: &dyn AdminAsks, names: &[String]) -> Result<HashMap<String, Option<String>>, Refusal> {
    let mut out = HashMap::new();
    for name in names {
        out.insert(name.clone(), env(asks, name)?);
    }
    Ok(out)
}

fn mock_mode(asks: &dyn AdminAsks) -> Result<bool, Refusal> {
    match asks.mode_read().map_err(host)? {
        Some(mode) => Ok(mode == "mock"),
        None => {
            let flags = env_map(asks, &[admin::MODE_ENV_VAR.to_string(), admin::MOCK_DATA_VAR.to_string()])?;
            Ok(admin::env_mock(&|name| flags.get(name).cloned().flatten()))
        }
    }
}

fn list(asks: &dyn AdminAsks, catalog: &Catalog, filter: Option<&str>) -> Result<Value, Refusal> {
    let view = store_view(asks)?;
    let store_first = admin::source_is_store(env(asks, admin::SOURCE_VAR)?.as_deref());
    let mock = mock_mode(asks)?;
    let names = admin::list_env_names(catalog, filter, view.as_ref(), store_first);
    let values = env_map(asks, &names)?;
    let lookup = |name: &str| values.get(name).cloned().flatten();
    let facts = ListFacts { store: view.as_ref(), store_first, env: &lookup, mock };
    Ok(admin::list_report(catalog, filter, &facts))
}

fn test(asks: &dyn AdminAsks, catalog: &Catalog, service: &str) -> Result<Value, Refusal> {
    let mut resolved: HashMap<String, bool> = HashMap::new();
    for name in admin::test_subjects(catalog, service) {
        let stored = asks.store_read(&name).map_err(host)?.filter(|value| !value.is_empty());
        let present = match stored {
            Some(_) => true,
            None => env(asks, &name)?.is_some(),
        };
        resolved.insert(name, present);
    }
    Ok(admin::test_report(catalog, service, &|name| resolved.get(name).copied().unwrap_or(false)))
}

fn counts(rotated: &RotateCounts) -> Value {
    Value::object(ValueMap::from_iter([
        ("rotated".to_string(), Value::Number(rotated.rotated as f64)),
        ("alreadyCurrent".to_string(), Value::Number(rotated.already_current as f64)),
        ("unreadable".to_string(), Value::Number(rotated.unreadable as f64)),
    ]))
}

/// Run one credential effect for `caller` (the dispatch's `@user`: its id and `role`). `catalog` builds the declared
/// credentials only for the effects that check them.
pub fn run(
    asks: &dyn AdminAsks,
    op: CredentialAdminOp,
    request: &Value,
    caller: Option<&ServiceCaller>,
    catalog: &dyn Fn() -> Result<Catalog, Refusal>,
) -> Result<Value, Refusal> {
    if op.gated() {
        let allowed = admin::admin_roles(env(asks, admin::ADMIN_ROLES_VAR)?.as_deref());
        admin::gate(op, caller.and_then(|caller| caller.role.as_deref()), &allowed)?;
    }
    let parsed = admin::parse_request(op, request)?;
    let missing = |name: &str| Refusal::plain(format!("{} needs {name}", op.name()));
    let service = || parsed.service.as_deref().ok_or_else(|| missing("service"));
    let env_var = || parsed.env_var.as_deref().ok_or_else(|| missing("envVar"));
    match op {
        CredentialAdminOp::List => list(asks, &catalog()?, parsed.service.as_deref()),
        CredentialAdminOp::Set => {
            let value = parsed.value.as_deref().ok_or_else(|| missing("value"))?;
            require_store(asks, " \u{2014} set credentials via the environment")?;
            admin::settable(&catalog()?, service()?, env_var()?)?;
            asks.store_write(service()?, env_var()?, value).map_err(host)?;
            Ok(admin::saved_report(service()?, env_var()?, value))
        }
        CredentialAdminOp::Remove => {
            require_store(asks, "")?;
            admin::settable(&catalog()?, service()?, env_var()?)?;
            let removed = asks.store_remove(env_var()?).map_err(host)?;
            Ok(Value::object(ValueMap::from_iter([("removed".to_string(), Value::Bool(removed))])))
        }
        CredentialAdminOp::Test => test(asks, &catalog()?, service()?),
        CredentialAdminOp::Mode => {
            let requested = parsed.mode.as_deref().ok_or_else(|| missing("mode"))?;
            admin::requested_mode(requested)?;
            let now = asks.mode_write(requested).map_err(host)?;
            Ok(Value::object(ValueMap::from_iter([("mode".to_string(), Value::String(now.into()))])))
        }
        CredentialAdminOp::Rotate => {
            require_store(asks, "")?;
            Ok(counts(&asks.store_rotate().map_err(host)?))
        }
    }
}

/// The services the `call-service` effects of `schema`'s transitions and ticks name, as `OirModule::invoked_services`
/// reads them off the lowered program. An effect that does not lower names nothing here; `orb validate` reports it.
pub fn schema_invoked_services(
    schema: &orbital_core_base::schema::types::OrbitalSchema,
) -> std::collections::BTreeSet<String> {
    use orbital_core_base::schema::types::TraitReference;
    let mut names = std::collections::BTreeSet::new();
    let mut visit = |effect: &orbital_core_base::schema::sexpr::SExpression| {
        if let Ok(lowered) = orbital_core_eval::lower::lower_sexpr(effect) {
            orbital_core_base::ir::invoked_services_in(&lowered, &mut names);
        }
    };
    for orbital in &schema.orbitals {
        for reference in &orbital.traits {
            let TraitReference::Inline(t) = reference else {
                continue;
            };
            t.state_machine
                .transitions
                .iter()
                .flat_map(|tr| tr.effects.iter().flatten())
                .for_each(&mut visit);
            t.ticks
                .iter()
                .flatten()
                .flat_map(|tick| &tick.effects)
                .for_each(&mut visit);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Host {
        env: BTreeMap<String, String>,
        store: Option<RefCell<Vec<(String, String, String)>>>,
        enabled: bool,
        mode: RefCell<Option<String>>,
        refuse_mock: bool,
        asked: RefCell<Vec<String>>,
    }

    impl Host {
        fn with_store(enabled: bool) -> Self {
            Self { store: Some(RefCell::new(Vec::new())), enabled, ..Self::default() }
        }
        fn env(mut self, name: &str, value: &str) -> Self {
            self.env.insert(name.to_string(), value.to_string());
            self
        }
        fn seed(&self, service: &str, name: &str, value: &str) {
            self.store.as_ref().unwrap().borrow_mut().push((service.into(), name.into(), value.into()));
        }
    }

    impl AdminAsks for Host {
        fn secret(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
            self.asked.borrow_mut().push(format!("secret:{name}:{}", principal.unwrap_or("-")));
            Ok(self.env.get(name).cloned())
        }
        fn store_list(&self) -> Result<Option<StoreListing>, String> {
            self.asked.borrow_mut().push("list".into());
            Ok(self.store.as_ref().map(|rows| StoreListing {
                enabled: self.enabled,
                entries: rows.borrow().iter().map(|(service, name, _)| StoredName { service: service.clone(), name: name.clone() }).collect(),
            }))
        }
        fn store_read(&self, name: &str) -> Result<Option<String>, String> {
            self.asked.borrow_mut().push(format!("read:{name}"));
            Ok(self.store.as_ref().and_then(|rows| rows.borrow().iter().find(|(_, n, _)| n == name).map(|(_, _, v)| v.clone())))
        }
        fn store_write(&self, service: &str, name: &str, value: &str) -> Result<(), String> {
            self.asked.borrow_mut().push(format!("write:{name}"));
            if !self.enabled {
                return Err("ALMADAR_CREDENTIAL_MASTER_KEY is not configured \u{2014} cannot store credentials".into());
            }
            let rows = self.store.as_ref().unwrap();
            rows.borrow_mut().retain(|(_, n, _)| n != name);
            rows.borrow_mut().push((service.into(), name.into(), value.into()));
            Ok(())
        }
        fn store_remove(&self, name: &str) -> Result<bool, String> {
            let rows = self.store.as_ref().unwrap();
            let before = rows.borrow().len();
            rows.borrow_mut().retain(|(_, n, _)| n != name);
            Ok(rows.borrow().len() != before)
        }
        fn store_rotate(&self) -> Result<RotateCounts, String> {
            if !self.enabled {
                return Err("ALMADAR_CREDENTIAL_MASTER_KEY is not configured \u{2014} cannot rotate".into());
            }
            Ok(RotateCounts { rotated: 0, already_current: self.store.as_ref().unwrap().borrow().len() as u64, unreadable: 0 })
        }
        fn mode_read(&self) -> Result<Option<String>, String> {
            Ok(self.mode.borrow().clone())
        }
        fn mode_write(&self, mode: &str) -> Result<String, String> {
            if self.mode.borrow().is_none() {
                return Err(admin::NO_MANAGER.to_string());
            }
            if mode == "mock" && self.refuse_mock {
                return Err("Mocked integrations are not permitted when NODE_ENV=production.".into());
            }
            *self.mode.borrow_mut() = Some(mode.to_string());
            Ok(mode.to_string())
        }
    }

    fn catalog_of() -> Catalog {
        Catalog::from_registry(
            r#"{ "services": { "apikeys": { "backend": "production", "credentials": [{ "envVar": "SALT", "required": true, "description": "salt" }] }, "hosting": { "backend": "production" } } }"#,
        )
        .unwrap()
    }

    fn object(pairs: &[(&str, &str)]) -> Value {
        Value::Object(pairs.iter().map(|(k, v)| (k.to_string(), Value::string(v.to_string()))).collect())
    }

    fn caller(role: Option<&str>) -> ServiceCaller {
        ServiceCaller { principal: "u1".into(), role: role.map(str::to_string) }
    }

    fn run_with(host: &Host, op: CredentialAdminOp, request: &Value, role: Option<&str>) -> Result<Value, Refusal> {
        run(host, op, request, Some(&caller(role)), &|| Ok(catalog_of()))
    }

    fn get<'a>(value: &'a Value, path: &str) -> &'a Value {
        value.get_path(path).unwrap_or_else(|| panic!("{path} missing in {value:?}"))
    }

    #[test]
    fn a_set_stores_through_the_host_and_answers_the_masked_tail_only() {
        let host = Host::with_store(true);
        let reply = run_with(&host, CredentialAdminOp::Set, &object(&[("service", "apikeys"), ("envVar", "SALT"), ("value", "salt-value-1111")]), Some("admin")).unwrap();
        assert_eq!(get(&reply, "last4"), &Value::string("1111"));
        assert!(!format!("{reply:?}").contains("salt-value"));
        assert_eq!(host.store.as_ref().unwrap().borrow().len(), 1);
    }

    #[test]
    fn a_refused_role_or_an_undeclared_name_never_reaches_the_store() {
        let host = Host::with_store(true);
        let refused = run_with(&host, CredentialAdminOp::Set, &object(&[("service", "apikeys"), ("envVar", "SALT"), ("value", "v")]), Some("viewer")).unwrap_err();
        assert_eq!(refused.code, Some(admin::FORBIDDEN_CODE));
        let undeclared = run_with(&host, CredentialAdminOp::Set, &object(&[("service", "apikeys"), ("envVar", "OTHER"), ("value", "v")]), Some("admin")).unwrap_err();
        assert!(undeclared.message.contains("is not a declared credential"));
        assert!(host.store.as_ref().unwrap().borrow().is_empty());
        assert!(!host.asked.borrow().iter().any(|ask| ask.starts_with("write:")), "no write was asked: {:?}", host.asked.borrow());
    }

    #[test]
    fn the_roles_come_from_the_hosts_environment_and_a_missing_store_is_named() {
        let host = Host::default().env(admin::ADMIN_ROLES_VAR, "ops");
        let request = object(&[("service", "apikeys"), ("envVar", "SALT"), ("value", "v")]);
        assert_eq!(run_with(&host, CredentialAdminOp::Set, &request, Some("admin")).unwrap_err().message, "forbidden: \"set\" requires one of roles: ops");
        assert_eq!(
            run_with(&host, CredentialAdminOp::Set, &request, Some("ops")).unwrap_err().message,
            "No credential store is installed on this host \u{2014} set credentials via the environment"
        );
        assert_eq!(
            run_with(&host, CredentialAdminOp::Remove, &object(&[("service", "apikeys"), ("envVar", "SALT")]), Some("ops")).unwrap_err().message,
            "No credential store is installed on this host"
        );
    }

    #[test]
    fn list_masks_stored_values_and_reads_the_environment_only_where_the_store_is_silent() {
        let host = Host::with_store(true).env("SALT", "env-salt-0042");
        host.seed("apikeys", "SALT", "stored-salt-9876");
        let reply = run_with(&host, CredentialAdminOp::List, &Value::object(ValueMap::default()), None).unwrap();
        let entry = &get(&reply, "entries").as_array().unwrap()[0];
        assert_eq!(get(entry, "source"), &Value::string("store"));
        assert_eq!(get(entry, "last4"), &Value::string("9876"));
        assert!(!host.asked.borrow().iter().any(|ask| ask == "secret:SALT:-"), "a stored name is not read from the environment");
        let bare = Host::with_store(true).env("SALT", "env-salt-0042");
        let reply = run_with(&bare, CredentialAdminOp::List, &Value::object(ValueMap::default()), None).unwrap();
        assert_eq!(get(&get(&reply, "entries").as_array().unwrap()[0], "last4"), &Value::string("0042"));
    }

    #[test]
    fn mode_is_validated_by_the_kernel_and_refused_or_applied_by_the_host() {
        let none = Host::default();
        assert_eq!(run(&none, CredentialAdminOp::Mode, &object(&[("mode", "hybrid")]), Some(&caller(Some("admin"))), &|| Ok(catalog_of())).unwrap_err().message, "mode must be \"mock\" or \"real\"");
        assert_eq!(run(&none, CredentialAdminOp::Mode, &object(&[("mode", "mock")]), Some(&caller(Some("admin"))), &|| Ok(catalog_of())).unwrap_err().message, admin::NO_MANAGER);
        let managed = Host { mode: RefCell::new(Some("real".into())), refuse_mock: true, ..Host::default() };
        assert_eq!(
            run(&managed, CredentialAdminOp::Mode, &object(&[("mode", "mock")]), Some(&caller(Some("admin"))), &|| Ok(catalog_of())).unwrap_err().message,
            "Mocked integrations are not permitted when NODE_ENV=production."
        );
        let reply = run(&managed, CredentialAdminOp::Mode, &object(&[("mode", "real")]), Some(&caller(Some("admin"))), &|| Ok(catalog_of())).unwrap();
        assert_eq!(get(&reply, "mode"), &Value::string("real"));
    }

    #[test]
    fn rotate_counts_come_from_the_host_and_need_a_store() {
        let host = Host::with_store(true);
        host.seed("apikeys", "SALT", "v");
        let reply = run(&host, CredentialAdminOp::Rotate, &Value::Null, Some(&caller(Some("owner"))), &|| Ok(catalog_of())).unwrap();
        assert_eq!(get(&reply, "alreadyCurrent"), &Value::Number(1.0));
        let none = Host::default();
        assert_eq!(run(&none, CredentialAdminOp::Rotate, &Value::Null, Some(&caller(Some("owner"))), &|| Ok(catalog_of())).unwrap_err().message, admin::NO_STORE);
        assert_eq!(run(&host, CredentialAdminOp::Rotate, &Value::Null, Some(&caller(Some("viewer"))), &|| Ok(catalog_of())).unwrap_err().code, Some(admin::FORBIDDEN_CODE));
    }

    #[test]
    fn test_is_presence_only_through_the_store_then_the_environment() {
        let host = Host::with_store(true);
        let absent = run_with(&host, CredentialAdminOp::Test, &object(&[("service", "apikeys")]), Some("admin")).unwrap();
        assert_eq!(get(&absent, "ok"), &Value::Bool(false));
        host.seed("apikeys", "SALT", "v");
        let stored = run_with(&host, CredentialAdminOp::Test, &object(&[("service", "apikeys")]), Some("admin")).unwrap();
        assert_eq!(get(&stored, "ok"), &Value::Bool(true));
        let env_only = Host::default().env("SALT", "v");
        assert_eq!(get(&run_with(&env_only, CredentialAdminOp::Test, &object(&[("service", "apikeys")]), Some("admin")).unwrap(), "ok"), &Value::Bool(true));
    }

    #[test]
    fn test_of_a_program_group_needs_one_of_its_declared_credentials_from_the_store_or_the_environment() {
        let group = || {
            Ok(catalog_of().with_program_credentials(&[
                orbital_core_base::secrets::ProgramCredential { group: "MailOrbital".into(), env_var: "RESEND".into(), description: String::new() },
                orbital_core_base::secrets::ProgramCredential { group: "MailOrbital".into(), env_var: "SENDGRID".into(), description: String::new() },
            ]))
        };
        let admin = Some(caller(Some("admin")));
        let request = object(&[("service", "MailOrbital")]);
        let empty = Host::with_store(true);
        let none = run(&empty, CredentialAdminOp::Test, &request, admin.as_ref(), &group).unwrap();
        assert_eq!(get(&none, "ok"), &Value::Bool(false));
        let stored = Host::with_store(true);
        stored.seed("MailOrbital", "SENDGRID", "v");
        assert_eq!(get(&run(&stored, CredentialAdminOp::Test, &request, admin.as_ref(), &group).unwrap(), "ok"), &Value::Bool(true));
        let env_only = Host::default().env("RESEND", "v");
        assert_eq!(get(&run(&env_only, CredentialAdminOp::Test, &request, admin.as_ref(), &group).unwrap(), "ok"), &Value::Bool(true));
    }

    #[test]
    fn the_services_a_program_calls_come_from_its_transitions_and_ticks() {
        let schema: orbital_core_base::schema::types::OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Billing",
                "entity": { "name": "Invoice", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Pay",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "PAY", "name": "Pay" }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "PAY",
                            "effects": [["do", ["call-service", "stripe", "charge", {}]]] }]
                    },
                    "ticks": [{ "name": "sync", "interval": "1m", "effects": [["call-service", "email", "send", {}]] }]
                }, {
                    "name": "Quiet",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "events": [], "transitions": [] }
                }]
            }]
        }))
        .unwrap();
        let called = schema_invoked_services(&schema);
        assert_eq!(
            called.iter().map(String::as_str).collect::<Vec<_>>(),
            ["email", "stripe"],
            "nested and tick effects count"
        );
    }
}
