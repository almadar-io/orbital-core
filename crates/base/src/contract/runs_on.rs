//! Where an effect executes — the Rust statement of the registry's `runsOn`
//! (`@almadar/std` `OperatorMeta.runsOn`). It lives here because the executor,
//! the validator and the shells all need it and `orbital-core` is the crate they
//! share; `orbital-compiler/tests/runs_on_drift.rs` fails the build when this
//! table and `canonical-operators.json` disagree.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::ir::effects::OirEffect;

/// Execution site of an effect operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunsOn {
    /// A client host renders or navigates it; a server ships it back.
    Client,
    /// A client host delegates it to the server.
    Server,
    /// Runs wherever it is dispatched.
    Any,
}

impl RunsOn {
    /// The registry spelling (`"client"`, `"server"`, `"any"`).
    pub fn as_str(self) -> &'static str {
        match self {
            RunsOn::Client => "client",
            RunsOn::Server => "server",
            RunsOn::Any => "any",
        }
    }
}

/// Every effect operator whose site is not `any`, by registry name.
pub const EFFECT_SITES: &[(&str, RunsOn)] = &[
    ("access-check", RunsOn::Server),
    ("browser/clipboard-read", RunsOn::Client),
    ("browser/clipboard-write", RunsOn::Client),
    ("browser/embed", RunsOn::Client),
    ("browser/geolocation-current", RunsOn::Client),
    ("browser/open-file-picker", RunsOn::Client),
    ("browser/page-facts", RunsOn::Client),
    ("browser/prompt", RunsOn::Client),
    ("browser/push-subscribe", RunsOn::Client),
    ("browser/tab-act", RunsOn::Client),
    ("browser/tab-apply", RunsOn::Client),
    ("browser/tab-apply-many", RunsOn::Client),
    ("browser/tab-capture", RunsOn::Client),
    ("browser/tab-close", RunsOn::Client),
    ("browser/tab-list", RunsOn::Client),
    ("browser/tab-navigate", RunsOn::Client),
    ("browser/tab-open", RunsOn::Client),
    ("browser/tab-query", RunsOn::Client),
    ("browser/tab-unwatch", RunsOn::Client),
    ("browser/tab-watch", RunsOn::Client),
    ("call-service", RunsOn::Server),
    ("cancel-call", RunsOn::Server),
    ("credential-forget", RunsOn::Server),
    ("credential-list", RunsOn::Server),
    ("credential-mode", RunsOn::Server),
    ("credential-remove", RunsOn::Server),
    ("credential-rotate", RunsOn::Server),
    ("credential-set", RunsOn::Server),
    ("credential-status", RunsOn::Server),
    ("credential-test", RunsOn::Server),
    ("fetch", RunsOn::Server),
    ("fetch-stream", RunsOn::Server),
    ("navigate", RunsOn::Client),
    ("navigate-back", RunsOn::Client),
    ("os/watch-cron", RunsOn::Server),
    ("os/watch-env", RunsOn::Server),
    ("os/watch-files", RunsOn::Server),
    ("os/watch-port", RunsOn::Server),
    ("os/watch-process", RunsOn::Server),
    ("os/watch-signal", RunsOn::Server),
    ("persist", RunsOn::Server),
    ("random-bytes", RunsOn::Server),
    ("render-ui", RunsOn::Client),
    ("send-server", RunsOn::Client),
];

/// Data effects whose site follows their target entity's residence: the index
/// of the argument naming the entity. Twin of the registry's
/// `entityArgPosition`; `runs_on_drift.rs` fails the build when they disagree.
pub const ENTITY_ARGS: &[(&str, usize)] = &[("fetch", 0), ("persist", 1)];

/// The program's browser-stored entities (`[persistent: x, browser]`), by name: the
/// one input every "where does this data effect run" question needs. A
/// `[runtime]` entity's frame lives in the client, but its rows stay in the
/// server's per-session store (Runtime Spec 9.2), so it is not listed here.
#[derive(Debug, Clone, Default)]
pub struct Residence {
    client: std::collections::HashSet<String>,
    /// Services whose only runtime is the browser: a call to one runs there.
    browser_services: std::collections::HashSet<String>,
}

impl Residence {
    /// No browser-stored entities: every data effect is server work.
    pub fn none() -> Self {
        Self::default()
    }

    /// The residence of a program's entities.
    pub fn of_entities<'a, E>(entities: impl IntoIterator<Item = &'a E>) -> Self
    where
        E: core::borrow::Borrow<crate::schema::types::EntityDefinition> + 'a,
    {
        Self {
            client: entities
                .into_iter()
                .map(core::borrow::Borrow::borrow)
                .filter(|e| e.browser)
                .map(|e| e.name.clone())
                .collect(),
            browser_services: Default::default(),
        }
    }

    /// The residence of an IR module's entities and browser-only services.
    pub fn of_module(module: &crate::ir::types::OirModule) -> Self {
        Self {
            client: module
                .entities
                .iter()
                .filter(|e| e.browser)
                .map(|e| e.name.clone())
                .collect(),
            browser_services: module.browser_services.iter().cloned().collect(),
        }
    }

    /// The same residence, with `services` declared browser-only.
    pub fn with_browser_services(mut self, services: impl IntoIterator<Item = String>) -> Self {
        self.browser_services.extend(services);
        self
    }

    /// Whether a call to `service` runs in the browser.
    pub fn is_browser_service(&self, service: &str) -> bool {
        self.browser_services.contains(service)
    }

    /// Whether `entity`'s rows live in the browser.
    pub fn is_client(&self, entity: &str) -> bool {
        self.client.contains(entity)
    }
}

/// Whether a service's declared runtimes (services registry `runtimes`)
/// include `runtime` (`node`, `browser`).
pub fn declares_runtime(runtimes: &[String], runtime: &str) -> bool {
    runtimes.iter().any(|r| r == runtime)
}

/// Whether a service's declared runtimes put its calls in the browser: it
/// declares the browser and no server runtime.
pub fn is_browser_only(runtimes: &[String]) -> bool {
    declares_runtime(runtimes, "browser") && !declares_runtime(runtimes, "node")
}

/// The compile error for a `call-service` its target cannot host.
pub const SERVICE_RUNTIME_UNSUPPORTED: &str = "ORB_SHELL_SERVICE_RUNTIME_UNSUPPORTED";

/// The argument position naming a data effect's entity, when it has one.
pub fn entity_arg(operator: &str) -> Option<usize> {
    ENTITY_ARGS
        .iter()
        .find(|(name, _)| *name == operator)
        .map(|(_, i)| *i)
}

/// Where one call runs: its registry site, except a data effect whose target
/// entity (a literal name, never a binding) lives in the client, a
/// `call-service` to a browser-only service runs there.
/// Twin of `@almadar/std` `effectSiteFor`.
pub fn effect_site(
    operator: &str,
    args: &[crate::schema::sexpr::SExpression],
    residence: &Residence,
) -> RunsOn {
    let site = runs_on(operator);
    if site != RunsOn::Server {
        return site;
    }
    if operator == "call-service" {
        return match args.first().and_then(|a| a.as_str()) {
            Some(service) if !service.starts_with('@') && residence.is_browser_service(service) => {
                RunsOn::Client
            }
            _ => RunsOn::Server,
        };
    }
    if operator == "persist" {
        if let Some(ops) = crate::persist_batch::batch_ops_arg(args) {
            return match crate::persist_batch::batch_placement(ops, residence) {
                crate::persist_batch::BatchPlacement::Client => RunsOn::Client,
                _ => RunsOn::Server,
            };
        }
    }
    match entity_arg(operator)
        .and_then(|i| args.get(i))
        .and_then(|a| a.as_str())
    {
        Some(entity) if !entity.starts_with('@') && residence.is_client(entity) => RunsOn::Client,
        _ => RunsOn::Server,
    }
}

/// Where an IR effect runs: [`effect_site`] for the IR's typed data effects.
pub fn ir_effect_site(effect: &OirEffect, residence: &Residence) -> RunsOn {
    if let OirEffect::CallService { service, .. } = effect {
        return if residence.is_browser_service(service) {
            RunsOn::Client
        } else {
            RunsOn::Server
        };
    }
    let site = effect_runs_on(effect);
    if site != RunsOn::Server || entity_arg(effect_operator(effect)).is_none() {
        return site;
    }
    if let OirEffect::PersistBatch { entities, .. } = effect {
        let all_client = !entities.is_empty() && entities.iter().all(|e| residence.is_client(e));
        return if all_client {
            RunsOn::Client
        } else {
            RunsOn::Server
        };
    }
    let entity = match effect {
        OirEffect::Fetch { entity_type, .. } | OirEffect::Persist { entity_type, .. } => {
            Some(entity_type.as_str())
        }
        _ => None,
    };
    match entity {
        Some(name) if residence.is_client(name) => RunsOn::Client,
        _ => RunsOn::Server,
    }
}

/// Site of an effect operator by registry name; `Any` for every effect not in
/// [`EFFECT_SITES`] (and for a name that is not an effect at all).
pub fn runs_on(operator: &str) -> RunsOn {
    runs_on_known(operator).unwrap_or(RunsOn::Any)
}

/// The operator sites a compiled program carries (`_metadata.operatorSites`), for a kernel with no installed
/// operator table (a browser).
static PROGRAM_SITES: crate::sync::LazyLock<crate::sync::RwLock<std::collections::BTreeMap<String, RunsOn>>> =
    crate::sync::LazyLock::new(|| crate::sync::RwLock::new(std::collections::BTreeMap::new()));

impl RunsOn {
    /// The site a registry or program table spells `text`.
    pub fn parse(text: &str) -> Option<RunsOn> {
        match text {
            "client" => Some(RunsOn::Client),
            "server" => Some(RunsOn::Server),
            "any" => Some(RunsOn::Any),
            _ => None,
        }
    }
}

/// Install the sites a compiled program carries; refuses a site that is not `client`, `server` or `any`.
pub fn install_operator_sites(sites: &std::collections::BTreeMap<String, String>) -> Result<(), String> {
    let mut parsed = std::collections::BTreeMap::new();
    for (operator, site) in sites {
        let site = RunsOn::parse(site).ok_or_else(|| format!("operatorSites: {operator}: `{site}` is not client, server or any"))?;
        parsed.insert(operator.clone(), site);
    }
    PROGRAM_SITES.write().extend(parsed);
    Ok(())
}

/// The site of `operator`, or `None` when neither the kernel's own tables, the installed program sites nor an
/// installed operator table can place it: a client kernel then refuses the effect instead of guessing `Any`.
pub fn runs_on_known(operator: &str) -> Option<RunsOn> {
    let registry = crate::language_data::operator_data()
        .ok()
        .map(|_| crate::language::registry::get_operator(operator).and_then(|op| op.runs_on.as_deref()));
    known_site(operator, &PROGRAM_SITES.read(), registry)
}

/// `registry`: `None` when no operator table is installed, else the operator's declared site if it has one.
fn known_site(
    operator: &str,
    installed: &std::collections::BTreeMap<String, RunsOn>,
    registry: Option<Option<&str>>,
) -> Option<RunsOn> {
    if let Some(op) = crate::os::OsOp::from_name(operator) {
        return Some(if op.is_kv() { RunsOn::Any } else { RunsOn::Server });
    }
    if let Some((_, site)) = EFFECT_SITES.iter().find(|(name, _)| *name == operator) {
        return Some(*site);
    }
    if let Some(site) = installed.get(operator) {
        return Some(*site);
    }
    registry.map(|declared| declared.and_then(RunsOn::parse).unwrap_or(RunsOn::Any))
}

/// The sites, from the installed operator table, of each effect operator in `operators` that the kernel's own
/// tables do not place: what a compiled client program carries so its kernel places every effect it runs.
pub fn operator_sites<'a>(operators: impl IntoIterator<Item = &'a str>) -> std::collections::BTreeMap<String, String> {
    operators
        .into_iter()
        .filter(|name| crate::os::OsOp::from_name(name).is_none() && !EFFECT_SITES.iter().any(|(site, _)| site == name))
        .filter_map(|name| {
            let op = crate::language::registry::try_get_operator(name)?;
            op.has_side_effects.then(|| (name.to_string(), op.runs_on.clone().unwrap_or_else(|| "any".to_string())))
        })
        .collect()
}

/// The `_metadata` a compiled client program carries for a browser kernel, which has no installed core: the sites of
/// the effects its `orbitals` call, the dispatch facts and the endpoint protocol table.
pub fn client_program_metadata(orbitals: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut heads = std::collections::BTreeSet::new();
    collect_call_heads(orbitals, &mut heads);
    let sites = operator_sites(heads.iter().map(String::as_str));
    let mut metadata = serde_json::Map::new();
    if !sites.is_empty() {
        metadata.insert("operatorSites".to_string(), serde_json::json!(sites));
    }
    if let Some(facts) = crate::language::registry::table_dispatch_facts() {
        metadata.insert("dispatchFacts".to_string(), serde_json::json!(facts));
    }
    if let Ok(rows) = crate::protocols::table() {
        metadata.insert("protocols".to_string(), serde_json::json!(rows));
    }
    metadata
}

/// Every string at the head of a JSON array under `value`: the operator of each call the program spells.
fn collect_call_heads(value: &serde_json::Value, heads: &mut std::collections::BTreeSet<String>) {
    match value {
        serde_json::Value::Array(items) => {
            if let Some(serde_json::Value::String(head)) = items.first() {
                heads.insert(head.clone());
            }
            items.iter().for_each(|item| collect_call_heads(item, heads));
        }
        serde_json::Value::Object(map) => map.values().for_each(|item| collect_call_heads(item, heads)),
        _ => {}
    }
}

/// Registry name of an IR effect (its `kind()` except where the IR spells it differently).
pub fn effect_operator(effect: &OirEffect) -> &'static str {
    match effect.kind() {
        "set-dynamic" => "set",
        kind => kind,
    }
}

/// Site of an IR effect.
pub fn effect_runs_on(effect: &OirEffect) -> RunsOn {
    runs_on(effect_operator(effect))
}

/// True when `expr`'s tree calls an effect whose site is `server`, at any
/// depth — the one predicate the dispatch rule (`runtime::dispatch_mode`) and
/// the validator's local-trait contract share.
///
/// Fold-aware: an `if`/`when` branch whose condition is statically false
/// (`SExpression::is_statically_false` — e.g. `@config.selfFetch` inlined to
/// `false`) can never run, so its effects are not server work.
pub fn sexpr_runs_server_effect(
    expr: &crate::schema::sexpr::SExpression,
    residence: &Residence,
) -> bool {
    use crate::schema::sexpr::SExpression;
    let recur = |e: &SExpression| sexpr_runs_server_effect(e, residence);
    match expr {
        SExpression::Array(arr) => {
            let head = arr.first().and_then(|e| e.as_str());
            if matches!(head, Some("if") | Some("when")) && arr.len() >= 3 {
                let then_live = !arr[1].is_statically_false();
                return (then_live && recur(&arr[2]))
                    || (head == Some("if") && arr.get(3).is_some_and(recur))
                    || (head == Some("when") && then_live && arr[3..].iter().any(recur));
            }
            // Binding positions name values: a `let` pair's name and a
            // `fn`/`lambda` param list are never calls.
            if head == Some("let") && arr.len() >= 2 {
                let bound_live = match &arr[1] {
                    SExpression::Array(pairs) => pairs.iter().any(|pair| match pair {
                        SExpression::Array(p) => p.iter().skip(1).any(recur),
                        other => recur(other),
                    }),
                    other => recur(other),
                };
                return bound_live || arr[2..].iter().any(recur);
            }
            if matches!(head, Some("fn")) && arr.len() >= 3 {
                return arr[2..].iter().any(recur);
            }
            matches!(head, Some(op) if effect_site(op, &arr[1..], residence) == RunsOn::Server)
                || arr.iter().any(recur)
        }
        SExpression::Object(obj) => obj.values().any(recur),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_program_carries_the_sites_of_its_effects_and_the_language_tables_a_browser_kernel_lacks() {
        let program = serde_json::json!([{ "traits": [{ "stateMachine": { "transitions": [
            { "effects": [["behavior/resolve", "@payload.value", {}], ["math/abs", 1]] }
        ] } }] }]);
        let metadata = client_program_metadata(&program);
        assert_eq!(metadata["operatorSites"], serde_json::json!({ "behavior/resolve": "server" }));
        assert!(metadata.contains_key("dispatchFacts"));
        assert!(metadata["protocols"].as_array().is_some_and(|rows| !rows.is_empty()));
    }

    #[test]
    fn control_a_program_calling_no_effect_carries_no_sites_but_still_the_tables() {
        let metadata = client_program_metadata(&serde_json::json!([]));
        assert!(!metadata.contains_key("operatorSites"));
        assert!(metadata.contains_key("dispatchFacts") && metadata.contains_key("protocols"));
    }

    #[test]
    fn the_table_is_sorted_and_names_each_operator_once() {
        let names: Vec<&str> = EFFECT_SITES.iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted);
        assert!(EFFECT_SITES.iter().all(|(_, s)| *s != RunsOn::Any));
    }

    #[test]
    fn unlisted_effects_and_non_effects_run_anywhere() {
        for op in [
            "emit", "set", "spawn", "log", "math/ln", "no/such",
        ] {
            assert_eq!(runs_on(op), RunsOn::Any, "{op}");
        }
        assert_eq!(runs_on("persist"), RunsOn::Server);
        assert_eq!(runs_on("render-ui"), RunsOn::Client);
    }

    #[test]
    fn an_effect_absent_from_the_core_table_takes_its_site_from_the_installed_registry() {
        assert!(!EFFECT_SITES.iter().any(|(n, _)| *n == "behavior/resolve"));
        assert_eq!(runs_on("behavior/resolve"), RunsOn::Server);
        assert_eq!(runs_on("program/eval"), RunsOn::Server);
        assert_eq!(runs_on("program/validate"), RunsOn::Server);
        assert_eq!(
            runs_on("math/abs"),
            RunsOn::Any,
            "control: a pure operator runs anywhere"
        );
    }

    #[test]
    fn cancel_call_runs_where_call_service_runs() {
        assert_eq!(runs_on("cancel-call"), runs_on("call-service"));
        assert_eq!(runs_on("cancel-call"), RunsOn::Server);
    }

    fn call(service: &str) -> Vec<crate::schema::sexpr::SExpression> {
        serde_json::from_value(serde_json::json!([service, "send", {}])).expect("args")
    }

    fn rt(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn only_a_service_declaring_the_browser_alone_is_browser_only() {
        assert!(is_browser_only(&rt(&["browser"])));
        assert!(
            !is_browser_only(&rt(&["node", "browser"])),
            "node+browser stays on the server"
        );
        assert!(!is_browser_only(&rt(&["node"])));
        assert!(
            !is_browser_only(&[]),
            "a service declaring no runtime is not browser-only"
        );
    }

    #[test]
    fn every_tab_effect_runs_on_the_client_and_a_neighbouring_name_does_not() {
        for op in crate::tab::TabOp::ALL {
            assert_eq!(
                EFFECT_SITES.iter().find(|(name, _)| *name == op.name()).map(|(_, site)| *site),
                Some(RunsOn::Client),
                "{}",
                op.name()
            );
        }
        assert!(!EFFECT_SITES.iter().any(|(name, _)| name.starts_with("browser/tab-") && crate::tab::TabOp::from_name(name).is_none()));
    }

    #[test]
    fn a_call_to_a_browser_only_service_runs_on_the_client() {
        let residence = Residence::none().with_browser_services(["browser-svc".to_string()]);
        assert_eq!(
            effect_site("call-service", &call("browser-svc"), &residence),
            RunsOn::Client
        );
        assert_eq!(
            effect_site("call-service", &call("webhook"), &residence),
            RunsOn::Server
        );
        assert_eq!(
            effect_site("call-service", &call("browser-svc"), &Residence::none()),
            RunsOn::Server,
            "control: no browser services declared"
        );
        assert_eq!(
            effect_site("call-service", &call("@config.service"), &residence),
            RunsOn::Server,
            "a binding is never a literal service name"
        );
    }

    #[test]
    fn the_ir_call_follows_the_same_rule() {
        use crate::ir::expr::OirExpr;
        let residence = Residence::none().with_browser_services(["browser-svc".to_string()]);
        let page_call = OirEffect::call_service("browser-svc", "openTab", OirExpr::null());
        let webhook_call = OirEffect::call_service("webhook", "send", OirExpr::null());
        assert_eq!(ir_effect_site(&page_call, &residence), RunsOn::Client);
        assert_eq!(ir_effect_site(&webhook_call, &residence), RunsOn::Server);
        assert!(
            OirExpr::effect(page_call.clone()).runs_browser_data_effect(&residence),
            "a browser-only call is a browser leg"
        );
        assert!(!OirExpr::effect(webhook_call).runs_browser_data_effect(&residence));
        assert!(!OirExpr::effect(page_call).runs_browser_data_effect(&Residence::none()));
    }

    #[test]
    fn a_browser_only_call_is_not_server_work_for_the_dispatch_rule() {
        let residence = Residence::none().with_browser_services(["browser-svc".to_string()]);
        let expr: crate::schema::sexpr::SExpression =
            serde_json::from_value(serde_json::json!(["call-service", "browser-svc", "openTab", {}]))
                .expect("expr");
        assert!(!sexpr_runs_server_effect(&expr, &residence));
        assert!(
            sexpr_runs_server_effect(&expr, &Residence::none()),
            "control: without the declaration it is server work"
        );
    }

    #[test]
    fn the_module_residence_carries_its_stamped_browser_services() {
        let mut module = crate::ir::types::OirModule::new("App", "1.0.0");
        module.browser_services = vec!["browser-svc".to_string()];
        assert!(Residence::of_module(&module).is_browser_service("browser-svc"));
        assert!(!Residence::of_module(&module).is_browser_service("webhook"));
    }

    #[test]
    fn a_program_site_places_an_operator_a_kernel_without_the_operator_table_cannot() {
        let installed = std::collections::BTreeMap::from([("behavior/catalog".to_string(), RunsOn::Server)]);
        assert_eq!(known_site("behavior/catalog", &installed, None), Some(RunsOn::Server));
        assert_eq!(known_site("fetch", &installed, None), Some(RunsOn::Server));
        assert_eq!(known_site("behavior/catalog", &std::collections::BTreeMap::new(), Some(Some("server"))), Some(RunsOn::Server));
    }

    #[test]
    fn control_an_operator_no_table_places_is_unknown_and_an_installed_table_says_any() {
        let none = std::collections::BTreeMap::new();
        assert_eq!(known_site("behavior/catalog", &none, None), None);
        assert_eq!(known_site("program/print", &none, Some(None)), Some(RunsOn::Any));
        let bad = std::collections::BTreeMap::from([("x/y".to_string(), "sideways".to_string())]);
        assert!(install_operator_sites(&bad).unwrap_err().contains("not client, server or any"));
    }

    #[test]
    fn ir_spellings_map_to_registry_names() {
        assert_eq!(effect_operator(&OirEffect::fetch("Task")), "fetch");
        assert_eq!(effect_runs_on(&OirEffect::fetch("Task")), RunsOn::Server);
        assert_eq!(effect_runs_on(&OirEffect::emit("X")), RunsOn::Any);
    }
}
