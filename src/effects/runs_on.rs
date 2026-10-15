//! Where an effect executes — the Rust statement of the registry's `runsOn`
//! (`@almadar/std` `OperatorMeta.runsOn`). It lives here because the executor,
//! the validator and the shells all need it and `orbital-core` is the crate they
//! share; `orbital-compiler/tests/runs_on_drift.rs` fails the build when this
//! table and `canonical-operators.json` disagree.

use crate::ir::OirEffect;

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
    ("atomic", RunsOn::Server),
    ("browser/clipboard-read", RunsOn::Client),
    ("browser/clipboard-write", RunsOn::Client),
    ("browser/geolocation-current", RunsOn::Client),
    ("browser/open-file-picker", RunsOn::Client),
    ("browser/push-subscribe", RunsOn::Client),
    ("call-service", RunsOn::Server),
    ("cancel-call", RunsOn::Server),
    ("fetch", RunsOn::Server),
    ("fetch-stream", RunsOn::Server),
    ("http", RunsOn::Server),
    ("navigate", RunsOn::Client),
    ("navigate-back", RunsOn::Client),
    ("os/debounce", RunsOn::Server),
    ("os/watch-cron", RunsOn::Server),
    ("os/watch-env", RunsOn::Server),
    ("os/watch-files", RunsOn::Server),
    ("os/watch-http", RunsOn::Server),
    ("os/watch-port", RunsOn::Server),
    ("os/watch-process", RunsOn::Server),
    ("os/watch-signal", RunsOn::Server),
    ("persist", RunsOn::Server),
    ("render-ui", RunsOn::Client),
    ("send-server", RunsOn::Client),
    ("swap", RunsOn::Server),
];

/// Data effects whose site follows their target entity's residence: the index
/// of the argument naming the entity. Twin of the registry's
/// `entityArgPosition`; `runs_on_drift.rs` fails the build when they disagree.
pub const ENTITY_ARGS: &[(&str, usize)] = &[("fetch", 0), ("persist", 1)];

/// The program's browser-stored entities (`[persistent: x, local]`), by name: the
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
    pub fn of_entities<'a>(
        entities: impl IntoIterator<Item = &'a crate::schema::EntityDefinition>,
    ) -> Self {
        Self {
            client: entities
                .into_iter()
                .filter(|e| e.local)
                .map(|e| e.name.clone())
                .collect(),
            browser_services: Default::default(),
        }
    }

    /// The residence of an IR module's entities and browser-only services.
    pub fn of_module(module: &crate::ir::OirModule) -> Self {
        Self {
            client: module
                .entities
                .iter()
                .filter(|e| e.local)
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
/// entity (a literal name, never a binding) lives in the client, or a
/// `call-service` to a browser-only service, runs there.
/// Twin of `@almadar/std` `effectSiteFor`.
pub fn effect_site(
    operator: &str,
    args: &[crate::schema::SExpression],
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
        if let Some(ops) = super::persist_batch::batch_ops_arg(args) {
            return match super::persist_batch::batch_placement(ops, residence) {
                super::persist_batch::BatchPlacement::Client => RunsOn::Client,
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
    if let Some((_, site)) = EFFECT_SITES.iter().find(|(name, _)| *name == operator) {
        return *site;
    }
    match crate::language::registry::try_get_operator(operator).and_then(|op| op.runs_on.as_deref())
    {
        Some("server") => RunsOn::Server,
        Some("client") => RunsOn::Client,
        _ => RunsOn::Any,
    }
}

/// Registry name of an IR effect (its `kind()` except where the IR spells it differently).
pub fn effect_operator(effect: &OirEffect) -> &'static str {
    match effect.kind() {
        "swap" => "swap",
        "set-dynamic" => "set",
        kind => kind,
    }
}

/// Site of an IR effect.
pub fn effect_runs_on(effect: &OirEffect) -> RunsOn {
    runs_on(effect_operator(effect))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "emit", "set", "ref", "watch", "spawn", "log", "math/ln", "no/such",
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

    fn call(service: &str) -> Vec<crate::schema::SExpression> {
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
    fn a_call_to_a_browser_only_service_runs_on_the_client() {
        let residence = Residence::none().with_browser_services(["analytics".to_string()]);
        assert_eq!(
            effect_site("call-service", &call("analytics"), &residence),
            RunsOn::Client
        );
        assert_eq!(
            effect_site("call-service", &call("github"), &residence),
            RunsOn::Server
        );
        assert_eq!(
            effect_site("call-service", &call("analytics"), &Residence::none()),
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
        use crate::ir::OirExpr;
        let residence = Residence::none().with_browser_services(["analytics".to_string()]);
        let analytics = OirEffect::call_service("analytics", "pageview", OirExpr::null());
        let github = OirEffect::call_service("github", "listIssues", OirExpr::null());
        assert_eq!(ir_effect_site(&analytics, &residence), RunsOn::Client);
        assert_eq!(ir_effect_site(&github, &residence), RunsOn::Server);
        assert!(
            OirExpr::effect(analytics.clone()).runs_browser_data_effect(&residence),
            "a browser-only call is a browser leg"
        );
        assert!(!OirExpr::effect(github).runs_browser_data_effect(&residence));
        assert!(!OirExpr::effect(analytics).runs_browser_data_effect(&Residence::none()));
    }

    #[test]
    fn a_browser_only_call_is_not_server_work_for_the_dispatch_rule() {
        let residence = Residence::none().with_browser_services(["analytics".to_string()]);
        let expr: crate::schema::SExpression = serde_json::from_value(serde_json::json!([
            "call-service",
            "analytics",
            "pageview",
            {}
        ]))
        .expect("expr");
        assert!(!sexpr_runs_server_effect(&expr, &residence));
        assert!(
            sexpr_runs_server_effect(&expr, &Residence::none()),
            "control: without the declaration it is server work"
        );
    }

    #[test]
    fn the_module_residence_carries_its_stamped_browser_services() {
        let mut module = crate::ir::OirModule::new("App", "1.0.0");
        module.browser_services = vec!["analytics".to_string()];
        assert!(Residence::of_module(&module).is_browser_service("analytics"));
        assert!(!Residence::of_module(&module).is_browser_service("github"));
    }

    #[test]
    fn ir_spellings_map_to_registry_names() {
        use crate::ir::OirExpr;
        let swap = OirEffect::Swap {
            entity_type: "Task".into(),
            id: OirExpr::string("t1"),
            transform: OirExpr::number(1.0),
        };
        assert_eq!(effect_operator(&swap), "swap");
        assert_eq!(effect_runs_on(&swap), RunsOn::Server);
        assert_eq!(effect_runs_on(&OirEffect::fetch("Task")), RunsOn::Server);
        assert_eq!(effect_runs_on(&OirEffect::emit("X")), RunsOn::Any);
    }
}

/// True when `expr`'s tree calls an effect whose site is `server`, at any
/// depth — the one predicate the dispatch rule (`runtime::dispatch_mode`) and
/// the validator's local-trait contract share.
///
/// Fold-aware: an `if`/`when` branch whose condition is statically false
/// (`SExpression::is_statically_false` — e.g. `@config.selfFetch` inlined to
/// `false`) can never run, so its effects are not server work.
pub fn sexpr_runs_server_effect(expr: &crate::schema::SExpression, residence: &Residence) -> bool {
    use crate::schema::SExpression;
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
            if matches!(head, Some("fn") | Some("lambda")) && arr.len() >= 3 {
                return arr[2..].iter().any(recur);
            }
            matches!(head, Some(op) if effect_site(op, &arr[1..], residence) == RunsOn::Server)
                || arr.iter().any(recur)
        }
        SExpression::Object(obj) => obj.values().any(recur),
        _ => false,
    }
}
