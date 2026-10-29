//! Lazy orbitals (EXEC-LOAD-2): a stub per `uses lazy` orbital, loaded by the first request or event that reaches it.

use super::*;
use orbital_core_base::schema::types::LazyPage;

/// A lazy orbital's program as its host read it: the resolved schema and the message catalogs written beside it.
pub struct LazyProgram {
    pub schema: OrbitalSchema,
    pub catalogs: HashMap<String, HashMap<String, String>>,
}

/// Where a lazy orbital's file is read from. A host installs one to declare that it loads lazy orbitals through the kernel;
/// a kernel without one registers no stubs (a browser client opens lazy pages through its own loader).
pub trait LazySource: Send + Sync {
    /// The program at `file`, a path relative to the folder of the resolved `.orb` the program was booted from.
    fn read(&self, file: &str) -> Result<LazyProgram, String>;
}

#[derive(Clone)]
struct Stub {
    schema: String,
    after: Option<String>,
    listeners_registered: bool,
    places: Vec<String>,
    orbital: String,
    file: String,
    pages: Vec<(String, Option<String>)>,
    traits: Vec<orbital_core_base::schema::types::LazyTrait>,
}

/// What a host does with a lazy program the kernel has just registered (its per-schema declarations).
pub type LazyObserver = dyn Fn(&RuntimeKernel, &OrbitalSchema) + Send + Sync;

/// The lazy orbitals not yet loaded, in declaration order, and the source that loads them.
#[derive(Default)]
pub(super) struct LazyOrbitals {
    source: RwLock<Option<Arc<dyn LazySource>>>,
    observer: RwLock<Option<Arc<LazyObserver>>>,
    pending: RwLock<Vec<Stub>>,
}

fn joined(importer_file: &str, orb_ref: &str) -> String {
    match importer_file.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{orb_ref}"),
        None => orb_ref.to_string(),
    }
}

impl LazyOrbitals {
    pub(super) fn copy_of(&self) -> Self {
        Self {
            source: RwLock::new(self.source.read().clone()),
            observer: RwLock::new(self.observer.read().clone()),
            pending: RwLock::new(self.pending.read().clone()),
        }
    }

    pub(super) fn holds_listeners_of(&self, trait_name: &str) -> bool {
        self.pending
            .read()
            .iter()
            .any(|s| s.traits.iter().any(|t| t.name == trait_name))
    }
}

impl RuntimeKernel {
    /// Declare that this kernel loads its program's lazy orbitals from `source` (EXEC-LOAD-2). Install it before the program is
    /// registered: only a registration made after it records stubs.
    pub fn set_lazy_source(&self, source: Arc<dyn LazySource>) {
        *self.lazy.source.write() = Some(source);
    }

    /// Run `observer` on each lazy program this kernel registers after the booted one.
    pub fn on_lazy_loaded(&self, observer: Arc<LazyObserver>) {
        *self.lazy.observer.write() = Some(observer);
    }

    /// The lazy orbitals not loaded yet, in declaration order.
    pub fn lazy_pending(&self) -> Vec<String> {
        self.lazy
            .pending
            .read()
            .iter()
            .map(|s| s.orbital.clone())
            .collect()
    }

    /// Record a stub for each lazy orbital `schema` lists; its listeners register in the registration loop, where the orbital's
    /// traits would have registered them ([`Self::register_stub_listeners`]). Returns the orbitals that must be loaded now: a lazy orbital that ticks or serves an endpoint.
    pub(super) fn register_lazy_stubs(
        &self,
        schema: &OrbitalSchema,
        importer_file: &str,
    ) -> Result<Vec<String>, KernelError> {
        if self.lazy.source.read().is_none() {
            return Ok(Vec::new());
        }
        let mut boot = Vec::new();
        let mut stubs: Vec<Stub> = Vec::new();
        for page in schema.lazy_pages.iter().flatten() {
            self.add_stub_page(&mut stubs, &mut boot, page, &schema.name, importer_file)?;
        }
        self.lazy.pending.write().extend(stubs);
        Ok(boot)
    }

    fn add_stub_page(
        &self,
        stubs: &mut Vec<Stub>,
        boot: &mut Vec<String>,
        page: &LazyPage,
        schema_name: &str,
        importer_file: &str,
    ) -> Result<(), KernelError> {
        if let Some(stub) = stubs.iter_mut().find(|s| s.orbital == page.orbital) {
            stub.pages.push((page.path.clone(), page.protocol.clone()));
            if page.protocol.is_none() {
                stub.places.extend(page.places.iter().cloned());
            }
            return Ok(());
        }
        if self
            .lazy
            .pending
            .read()
            .iter()
            .any(|s| s.orbital == page.orbital)
            || self.registry.schemas.read().contains_key(&page.orbital)
        {
            return Ok(());
        }
        let serves_endpoint = page.protocol.as_deref().is_some_and(|protocol| {
            orbital_core_base::protocols::protocol_after_preflight(protocol)
                .is_some_and(|spec| spec.side != orbital_core_base::protocols::ProtocolSide::Client)
        });
        if serves_endpoint || page.traits.iter().any(|t| t.background_ticks) {
            boot.push(page.orbital.clone());
        }
        let places = if page.protocol.is_none() { page.places.clone() } else { Vec::new() };
        stubs.push(Stub {
            schema: schema_name.to_string(),
            after: page.after.clone(),
            listeners_registered: false,
            places,
            orbital: page.orbital.clone(),
            file: joined(importer_file, &page.orb_ref),
            pages: vec![(page.path.clone(), page.protocol.clone())],
            traits: page.traits.clone(),
        });
        Ok(())
    }

    /// Register the listeners of the pending stubs of `schema` that follow orbital `after` (`None`: those that were first),
    /// so a lazy orbital's listeners hold the position its eager twin's would.
    pub(super) fn register_stub_listeners(
        &self,
        schema: &str,
        after: Option<&str>,
    ) -> Result<(), KernelError> {
        let mut pending = self.lazy.pending.write();
        for stub in pending.iter_mut().filter(|s| {
            !s.listeners_registered && s.schema == schema && s.after.as_deref() == after
        }) {
            for lazy_trait in &stub.traits {
                for listen in &lazy_trait.listens {
                    self.listener_manager
                        .register(&lazy_trait.name, &stub.orbital, listen)
                        .map_err(KernelError::Internal)?;
                }
            }
            stub.listeners_registered = true;
        }
        Ok(())
    }

    /// Refuse a program whose lazy orbital follows an orbital the program does not hold.
    pub(super) fn require_stub_listeners_placed(&self, schema: &str) -> Result<(), KernelError> {
        match self
            .lazy
            .pending
            .read()
            .iter()
            .find(|s| !s.listeners_registered && s.schema == schema)
        {
            Some(stub) => Err(KernelError::Internal(format!(
                "program '{schema}': lazy orbital `{}` follows `{}`, which the program does not hold",
                stub.orbital,
                stub.after.as_deref().unwrap_or("")
            ))),
            None => Ok(()),
        }
    }

    /// The dispatch mode a pending lazy trait declared, `None` for a trait no pending stub owns.
    pub(super) fn lazy_trait_mode(&self, trait_name: &str) -> Option<DispatchMode> {
        self.lazy.pending.read().iter().find_map(|s| {
            s.traits
                .iter()
                .find(|t| t.name == trait_name)
                .map(|t| t.dispatch_mode)
        })
    }

    /// The traits pending lazy orbitals place on a route page.
    pub(super) fn lazy_routed_traits(&self) -> Vec<String> {
        self.lazy
            .pending
            .read()
            .iter()
            .flat_map(|s| s.places.iter().cloned())
            .collect()
    }

    /// The pending stubs and source of `parent` (a fork's); the fork's listener order follows the parent's.
    pub(super) fn inherit_lazy_stubs(&self, parent: &RuntimeKernel) -> Result<(), KernelError> {
        let copy = parent.lazy.copy_of();
        for stub in copy.pending.read().iter() {
            for lazy_trait in &stub.traits {
                for listen in &lazy_trait.listens {
                    self.listener_manager
                        .register(&lazy_trait.name, &stub.orbital, listen)
                        .map_err(KernelError::Internal)?;
                }
            }
        }
        *self.lazy.source.write() = copy.source.read().clone();
        *self.lazy.observer.write() = copy.observer.read().clone();
        *self.lazy.pending.write() = copy.pending.read().clone();
        self.listener_manager
            .follow_order(&parent.listener_manager.order());
        Ok(())
    }

    /// Load the lazy orbital `orbital` if it is still pending: read its file, register it as any program, merge its catalogs.
    pub fn ensure_orbital_loaded(&self, orbital: &str) -> Result<(), KernelError> {
        if !self
            .lazy
            .pending
            .read()
            .iter()
            .any(|s| s.orbital == orbital)
        {
            return Ok(());
        }
        let Some(stub) = self
            .lazy
            .pending
            .read()
            .iter()
            .find(|s| s.orbital == orbital)
            .cloned()
        else {
            return Ok(());
        };
        let failed = |reason: String| KernelError::LazyLoad {
            orbital: stub.orbital.clone(),
            reason,
        };
        let source = self
            .lazy
            .source
            .read()
            .clone()
            .ok_or_else(|| failed("the kernel has no lazy source".into()))?;
        let program = source.read(&stub.file).map_err(&failed)?;
        if !program
            .schema
            .orbitals
            .iter()
            .any(|o| o.name == stub.orbital)
        {
            return Err(failed(format!(
                "{} declares no orbital `{}`; it holds [{}]",
                stub.file,
                stub.orbital,
                program
                    .schema
                    .orbitals
                    .iter()
                    .map(|o| o.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let schema_name = program.schema.name.clone();
        self.register_schema_from(program.schema, &stub.file)?;
        let observer = self.lazy.observer.read().clone();
        if let (Some(observer), Some(schema)) = (observer, self.registered_schema(&schema_name)) {
            observer(self, &schema);
        }
        {
            let mut i18n = self.i18n.write();
            for (locale, catalog) in program.catalogs {
                i18n.catalogs.entry(locale).or_default().extend(catalog);
            }
        }
        info!(orbital = %stub.orbital, file = %stub.file, "loaded lazy orbital");
        Ok(())
    }

    /// Load the lazy orbital that owns `trait_name`, when there is one still pending.
    pub fn ensure_trait_loaded(&self, trait_name: &str) -> Result<(), KernelError> {
        let owner = self
            .lazy
            .pending
            .read()
            .iter()
            .find(|s| s.traits.iter().any(|t| t.name == trait_name))
            .map(|s| s.orbital.clone());
        match owner {
            Some(orbital) => self.ensure_orbital_loaded(&orbital),
            None => Ok(()),
        }
    }

    /// Load every pending lazy orbital with a route page matching `route_path`.
    /// Open `route_path`: load the pending lazy orbitals with a route page matching it, so [`Self::route_mounts`] lists their traits.
    pub fn ensure_route_loaded(&self, route_path: &str) -> Result<(), KernelError> {
        let owners: Vec<String> = self
            .lazy
            .pending
            .read()
            .iter()
            .filter(|s| {
                s.pages.iter().any(|(path, protocol)| {
                    protocol.is_none()
                        && orbital_core_base::route::path_matches_pattern(route_path, path)
                })
            })
            .map(|s| s.orbital.clone())
            .collect();
        owners
            .iter()
            .try_for_each(|orbital| self.ensure_orbital_loaded(orbital))
    }

    /// Drop the stubs `schema` resolves: its orbitals are registered now.
    pub(super) fn resolve_lazy_stubs(&self, schema: &OrbitalSchema) {
        let mut pending = self.lazy.pending.write();
        if !pending.is_empty() {
            pending.retain(|s| !schema.orbitals.iter().any(|o| o.name == s.orbital));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_source::InMemoryEntityCache;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn emitter() -> serde_json::Value {
        serde_json::json!({
            "name": "Emitter",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "PING", {}]] }]
            },
            "emits": [{ "event": "PING" }]
        })
    }

    fn hear(ticks: bool) -> serde_json::Value {
        let mut def = serde_json::json!({
            "name": "Hear",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD", "effects": [] }]
            },
            "listens": [{ "event": "PING", "triggers": "HEARD" }]
        });
        if ticks {
            def["ticks"] = serde_json::json!([{ "name": "beat", "interval": 1000, "runsInBackground": true, "effects": [] }]);
        }
        def
    }

    fn orbital(
        name: &str,
        entity: &str,
        trait_def: serde_json::Value,
        pages: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "entity": { "name": entity, "fields": [{ "name": "id", "type": "string" }] },
            "traits": [trait_def],
            "pages": pages
        })
    }

    fn program(orbitals: Vec<serde_json::Value>, lazy_pages: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App", "typeContractVersion": 1, "orbitals": orbitals, "lazyPages": lazy_pages
        }))
        .unwrap()
    }

    fn late_page(ticks: bool) -> serde_json::Value {
        let listens = serde_json::json!([{ "event": "PING", "triggers": "HEARD" }]);
        serde_json::json!([{
            "path": "/late", "orbital": "Late", "orbRef": "lazy/Late.orb",
            "traits": [{ "name": "Hear", "dispatchMode": "persistedAwaited", "listens": listens, "backgroundTicks": ticks }]
        }])
    }

    fn late_program(ticks: bool) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "Late", "typeContractVersion": 1,
            "orbitals": [orbital("Late", "Memo", hear(ticks), serde_json::json!([{ "name": "LatePage", "path": "/late", "traits": [{ "ref": "Hear" }] }]))]
        }))
        .unwrap()
    }

    struct Files {
        reads: AtomicUsize,
        ticks: bool,
        missing: bool,
    }

    impl LazySource for Files {
        fn read(&self, file: &str) -> Result<LazyProgram, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.missing {
                return Err(format!("{file}: no such file"));
            }
            assert_eq!(file, "lazy/Late.orb");
            Ok(LazyProgram {
                schema: late_program(self.ticks),
                catalogs: HashMap::new(),
            })
        }
    }

    fn kernel_with(source: Option<Arc<Files>>, ticks: bool) -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        if let Some(source) = source {
            kernel.set_lazy_source(source);
        }
        let main = orbital(
            "Main",
            "Note",
            emitter(),
            serde_json::json!([{ "name": "Home", "path": "/", "traits": [{ "ref": "Emitter" }] }]),
        );
        kernel
            .register_schema(program(vec![main], late_page(ticks)))
            .unwrap();
        kernel
    }

    fn files(ticks: bool, missing: bool) -> Arc<Files> {
        Arc::new(Files {
            reads: AtomicUsize::new(0),
            ticks,
            missing,
        })
    }

    #[test]
    fn a_stub_stands_for_the_orbital_until_something_reaches_it() {
        let source = files(false, false);
        let kernel = kernel_with(Some(source.clone()), false);
        assert_eq!(kernel.lazy_pending(), ["Late"]);
        assert!(kernel.registered_schema("Late").is_none());
        assert_eq!(
            source.reads.load(Ordering::SeqCst),
            0,
            "boot reads no lazy file"
        );
        let control = kernel_with(None, false);
        assert!(
            control.lazy_pending().is_empty(),
            "control: a kernel with no lazy source records no stub"
        );
    }

    #[test]
    fn the_first_request_for_a_lazy_route_loads_the_orbital_and_mounts_its_traits() {
        let source = files(false, false);
        let kernel = kernel_with(Some(source.clone()), false);
        kernel.ensure_route_loaded("/other").unwrap();
        assert_eq!(
            source.reads.load(Ordering::SeqCst),
            0,
            "control: a route no lazy page declares loads nothing"
        );
        assert!(
            kernel.route_mounts("/late").is_empty(),
            "before the load the route lists nothing"
        );
        kernel.ensure_route_loaded("/late").unwrap();
        kernel.ensure_route_loaded("/late").unwrap();
        assert_eq!(source.reads.load(Ordering::SeqCst), 1, "loaded once");
        assert!(kernel.lazy_pending().is_empty());
        assert_eq!(
            kernel.route_mounts("/late").len(),
            0,
            "Hear handles no lifecycle event, so it is not seeded"
        );
        assert!(kernel.trait_snapshot(None).contains_key("Hear"));
    }

    #[test]
    fn an_event_a_lazy_orbital_listens_to_loads_it_and_is_delivered_like_an_eager_one() {
        let source = files(false, false);
        let lazy = kernel_with(Some(source.clone()), false);
        let delivered = lazy.dispatch("Emitter", None, "GO", Value::Null).unwrap();
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
        assert_eq!(lazy.get_state("Hear", None).unwrap().current_state, "heard");

        let eager = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let main = orbital(
            "Main",
            "Note",
            emitter(),
            serde_json::json!([{ "name": "Home", "path": "/", "traits": [{ "ref": "Emitter" }] }]),
        );
        let late = orbital("Late", "Memo", hear(false), serde_json::json!([]));
        eager
            .register_schema(program(vec![main, late], serde_json::json!([])))
            .unwrap();
        let expected = eager.dispatch("Emitter", None, "GO", Value::Null).unwrap();
        assert_eq!(
            eager.get_state("Hear", None).unwrap().current_state,
            "heard"
        );
        let names = |outcomes: &[(String, ProcessEventResult)]| {
            outcomes.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            names(&delivered),
            names(&expected),
            "the same traits step in the same order"
        );
    }

    #[test]
    fn a_direct_dispatch_to_a_lazy_trait_loads_it() {
        let source = files(false, false);
        let kernel = kernel_with(Some(source.clone()), false);
        kernel.dispatch("Hear", None, "HEARD", Value::Null).unwrap();
        assert_eq!(
            kernel.get_state("Hear", None).unwrap().current_state,
            "heard"
        );
        let nobody = kernel.dispatch("Nobody", None, "HEARD", Value::Null);
        assert!(
            !matches!(nobody, Err(KernelError::LazyLoad { .. })),
            "control: a trait no stub owns loads nothing"
        );
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_lazy_orbital_that_ticks_is_loaded_when_the_program_registers() {
        let source = files(true, false);
        let kernel = kernel_with(Some(source.clone()), true);
        assert!(kernel.lazy_pending().is_empty());
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
        let quiet = files(false, false);
        let control = kernel_with(Some(quiet.clone()), false);
        assert_eq!(
            control.lazy_pending(),
            ["Late"],
            "control: one that does not tick stays pending"
        );
    }

    #[test]
    fn a_lazy_file_that_cannot_be_read_fails_naming_the_orbital() {
        let kernel = kernel_with(Some(files(false, true)), false);
        let error = kernel
            .dispatch("Hear", None, "HEARD", Value::Null)
            .unwrap_err();
        match error {
            KernelError::LazyLoad { orbital, reason } => {
                assert_eq!(orbital, "Late");
                assert!(reason.contains("lazy/Late.orb"), "{reason}");
            }
            other => panic!("expected LazyLoad, got {other}"),
        }
        assert_eq!(
            kernel.lazy_pending(),
            ["Late"],
            "a failed load leaves the stub in place"
        );
        assert!(
            kernel.dispatch("Emitter", None, "GO", Value::Null).is_err(),
            "the delivery to the lazy listener fails, it is not dropped"
        );
    }

    #[test]
    fn a_fork_inherits_the_stubs_and_loads_on_its_own() {
        let source = files(false, false);
        let kernel = kernel_with(Some(source.clone()), false);
        let fork = kernel
            .fork_session(
                Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
                KernelConfig::default(),
            )
            .unwrap();
        assert_eq!(fork.lazy_pending(), ["Late"]);
        fork.dispatch("Emitter", None, "GO", Value::Null).unwrap();
        assert_eq!(fork.get_state("Hear", None).unwrap().current_state, "heard");
        assert_eq!(
            kernel.lazy_pending(),
            ["Late"],
            "the parent is not changed by its fork"
        );
    }

    fn listener_orbital(name: &str, trait_name: &str) -> serde_json::Value {
        let mut def = hear(false);
        def["name"] = serde_json::json!(trait_name);
        orbital(name, &format!("{name}Row"), def, serde_json::json!([]))
    }

    fn fan_out(kernel: &RuntimeKernel) -> Vec<String> {
        let outcomes = kernel.dispatch("Emitter", None, "GO", Value::Null).unwrap();
        outcomes.into_iter().map(|(trait_name, _)| trait_name).collect()
    }

    #[test]
    fn a_lazy_orbitals_listener_keeps_the_place_its_eager_twin_has_in_the_fan_out() {
        let main = || orbital("Main", "Note", emitter(), serde_json::json!([{ "name": "Home", "path": "/", "traits": [{ "ref": "Emitter" }] }]));
        let others = || [listener_orbital("Before", "Pre"), listener_orbital("After", "Post")];
        for (index, after) in [(0usize, None), (1, Some("Main")), (2, Some("Before")), (3, Some("After"))] {
            let mut eager_order = vec![main(), others()[0].clone(), others()[1].clone()];
            eager_order.insert(index, orbital("Late", "Memo", hear(false), serde_json::json!([])));
            let eager = RuntimeKernel::new(
                RuntimeEnvironment::Server,
                Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
                KernelConfig::default(),
            );
            eager.register_schema(program(eager_order, serde_json::json!([]))).unwrap();
            let expected = fan_out(&eager);
            assert_eq!(expected.len(), 4, "the emitter and three listeners step: {expected:?}");

            let lazy = RuntimeKernel::new(
                RuntimeEnvironment::Server,
                Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
                KernelConfig::default(),
            );
            lazy.set_lazy_source(files(false, false));
            let mut page = late_page(false);
            if let Some(after) = after {
                page[0]["after"] = serde_json::json!(after);
            }
            lazy.register_schema(program(vec![main(), others()[0].clone(), others()[1].clone()], page)).unwrap();
            assert_eq!(fan_out(&lazy), expected, "lazy orbital at position {index}");
        }
    }

    #[test]
    fn a_lazy_orbital_that_follows_an_orbital_the_program_lacks_is_refused() {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel.set_lazy_source(files(false, false));
        let mut page = late_page(false);
        page[0]["after"] = serde_json::json!("Ghost");
        let main = orbital("Main", "Note", emitter(), serde_json::json!([]));
        let refused = kernel.register_schema(program(vec![main], page)).unwrap_err();
        assert!(refused.to_string().contains("Ghost"), "{refused}");
    }

    #[test]
    fn a_client_only_lazy_trait_on_a_route_never_triggers_a_server_load() {
        let source = files(false, false);
        let kernel = kernel_with(Some(source.clone()), false);
        let mut client = late_page(false);
        client[0]["traits"][0]["dispatchMode"] = serde_json::json!("hybridClientOnly");
        client[0]["places"] = serde_json::json!(["Hear"]);
        let main = orbital("Main", "Note", emitter(), serde_json::json!([]));
        let fresh = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        fresh.set_lazy_source(source.clone());
        fresh.register_schema(program(vec![main], client)).unwrap();
        fresh.dispatch("Emitter", None, "GO", Value::Null).unwrap();
        assert_eq!(source.reads.load(Ordering::SeqCst), 0, "a [client] trait on a route is the browser's, not the server's");
        assert_eq!(fresh.lazy_pending(), ["Late"]);

        let mut unrouted = late_page(false);
        unrouted[0]["traits"][0]["dispatchMode"] = serde_json::json!("hybridClientOnly");
        let main = orbital("Main", "Note", emitter(), serde_json::json!([]));
        let boot = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        boot.set_lazy_source(source.clone());
        boot.register_schema(program(vec![main], unrouted)).unwrap();
        boot.dispatch("Emitter", None, "GO", Value::Null).unwrap();
        assert_eq!(source.reads.load(Ordering::SeqCst), 1, "control: a [client] trait no route places is the host's, so it loads");

        kernel.dispatch("Emitter", None, "GO", Value::Null).unwrap();
        assert_eq!(source.reads.load(Ordering::SeqCst), 2, "control: a server trait loads");
    }

    #[test]
    fn a_fork_fans_out_in_its_parents_order() {
        let main = orbital("Main", "Note", emitter(), serde_json::json!([]));
        let mut page = late_page(false);
        page[0]["after"] = serde_json::json!("Before");
        let parent = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        parent.set_lazy_source(files(false, false));
        parent.register_schema(program(vec![main, listener_orbital("Before", "Pre"), listener_orbital("After", "Post")], page)).unwrap();
        let fork = parent
            .fork_session(Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>, KernelConfig::default())
            .unwrap();
        assert_eq!(fan_out(&fork), fan_out(&parent));
    }

    #[test]
    fn a_nested_lazy_file_sits_beside_the_file_that_lists_it() {
        assert_eq!(joined("", "lazy/A.orb"), "lazy/A.orb");
        assert_eq!(joined("lazy/A.orb", "lazy/B.orb"), "lazy/lazy/B.orb");
    }
}
