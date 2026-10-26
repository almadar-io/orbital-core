//! Registering a program: schema, traits, listeners, stores, credentials, mounts, and the per-request fork.

use super::*;

impl RuntimeKernel {
    /// Traits entering the mount: routed deliveries to them wait for their own lifecycle event.
    pub fn mount_traits<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut mount = self.mount.write();
        for t in traits {
            mount.awaiting.insert(t.into());
        }
    }

    /// A new mount replacing the previous one (a page switch): nothing held for
    /// the old mount survives, and `traits` await their lifecycle event.
    pub fn remount_traits<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut mount = self.mount.write();
        mount.awaiting.clear();
        mount.held.clear();
        self.renders.write().clear();
        for t in traits {
            mount.awaiting.insert(t.into());
        }
    }

    /// Traits leaving the mount: nothing stays held for them.
    pub fn unmount_traits<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut mount = self.mount.write();
        let mut renders = self.renders.write();
        for t in traits {
            let t = t.into();
            mount.awaiting.remove(&t);
            mount.held.remove(&t);
            renders.forget_traits([t.as_str()]);
        }
    }

    /// Create a kernel with a trigger callback for listener events.
    pub fn with_trigger_callback(mut self, callback: TriggerCallback) -> Self {
        self.listener_manager =
            ListenerManager::new(self.event_bus.clone()).with_trigger_callback(callback);
        self
    }

    /// The credentials every registered program declares (`secret` knobs its `http` auth, `credential-status` and `hash/scrypt` salt slots
    /// read), by registered schema name: the program half of the catalog the credential effects check names against.
    pub(super) fn program_credentials(
        &self,
    ) -> Result<Vec<orbital_core_base::secrets::ProgramCredential>, String> {
        let schemas = self.schemas.read();
        let mut names: Vec<&String> = schemas.keys().collect();
        names.sort();
        let mut found: Vec<orbital_core_base::secrets::ProgramCredential> = Vec::new();
        let mut add = |credential: orbital_core_base::secrets::ProgramCredential| {
            if !found
                .iter()
                .any(|c| c.group == credential.group && c.env_var == credential.env_var)
            {
                found.push(credential);
            }
        };
        for name in names {
            let program = serde_json::to_value(schemas[name].as_ref())
                .map_err(|e| format!("schema `{name}`: {e}"))?;
            orbital_core_base::secrets::program_credentials(&program)
                .into_iter()
                .for_each(&mut add);
        }
        self.managed_credentials
            .read()
            .iter()
            .cloned()
            .for_each(add);
        Ok(found)
    }

    /// Name the program whose credentials this kernel's credential effects manage beside its own (an integrations
    /// console serving the app a host shows): its declared credentials join the catalog, replacing the last one named.
    pub fn set_managed_program(&self, program: &serde_json::Value) {
        *self.managed_credentials.write() =
            orbital_core_base::secrets::program_credentials(program);
    }

    /// The credential catalog: the registry's services plus the credentials every registered program declares, what a
    /// `store`-source boot seeds the credential store with.
    pub fn credential_catalog(
        &self,
    ) -> Result<orbital_core_base::credential_admin::Catalog, String> {
        let program = self.program_credentials()?;
        orbital_core_base::credential_admin::catalog(&program).map_err(|refusal| refusal.message)
    }

    /// The `egress` origins every registered program declares, unioned: what a host's transport may reach.
    pub fn egress(&self) -> Vec<String> {
        self.egress.read().clone()
    }

    /// The `os/` grants every registered program declares, unioned: what a host builds its `OsHost` from.
    pub fn os_grants(&self) -> OsGrants {
        self.os_grants.read().clone()
    }

    /// Register a schema with the kernel.
    pub fn register_schema(&self, mut schema: OrbitalSchema) -> Result<(), KernelError> {
        schema.bind_unlinked_traits_to_host_entity();
        if let Some(declared) = &schema.egress {
            let mut egress = self.egress.write();
            for origin in declared {
                if !egress.contains(origin) {
                    egress.push(origin.clone());
                }
            }
        }
        self.os_grants.write().union(&schema.program_os_grants());
        self.register_placement(&schema);
        let schema_name = schema.name.clone();
        info!(schema = %schema_name, "Registering schema");
        self.theme_presets
            .write()
            .merge(ThemePresets::from_definitions(
                schema.theme_definitions.iter().flatten(),
            ));
        if let Some(sites) = schema.metadata.as_ref().and_then(|m| m.operator_sites.as_ref()) {
            orbital_core_base::runs_on::install_operator_sites(sites).map_err(KernelError::Internal)?;
        }
        match schema.metadata.as_ref().and_then(|m| m.protocols.clone()) {
            Some(rows) => orbital_core_base::protocols::install_program_protocols(rows).map_err(KernelError::Internal)?,
            None => {
                if let Err(e) = orbital_core_base::protocols::table() {
                    return Err(KernelError::Internal(format!(
                        "schema '{}': {}: {e}; the program carries no `_metadata.protocols`; compile it as a client program",
                        schema.name,
                        e.code()
                    )));
                }
            }
        }
        match schema.metadata.as_ref().and_then(|m| m.dispatch_facts.clone()) {
            Some(facts) => orbital_core_base::language::registry::install_dispatch_facts(facts).map_err(KernelError::Internal)?,
            None if orbital_core_base::language::registry::dispatch_facts().is_none() => {
                return Err(KernelError::Internal(format!(
                    "schema '{schema_name}': no operator table is installed and the program carries no `_metadata.dispatchFacts`; compile it as a client program"
                )));
            }
            None => {}
        }

        // A bare `ALMADAR_PERSONA` (id/role) resolves against this app's declared
        // roster — its [identity] entity's authored instances — which construction could
        // not see. Only fills a still-unset viewer: a later registration must not
        // clobber a viewer somebody already resolved or set.
        if let Ok(crate::persona::EnvPersona::Bare(spec)) = crate::persona::persona_from_env_spec()
        {
            if matches!(self.get_current_user(), Value::Null) {
                let roster = crate::persona::identity_roster(&schema);
                let user = crate::persona::resolve_persona_spec(&spec, &roster)
                    .map_err(KernelError::Internal)?;
                self.set_current_user(user);
            }
        }

        // Populate the entity access table from every declared entity's four
        // directives (additive across registrations — a later schema never
        // clobbers an earlier one's policies for an entity it doesn't itself
        // redeclare).
        {
            let mut table = self.entity_policies.write();

            for orbital in &schema.orbitals {
                for entity_ref in std::iter::once(&orbital.entity)
                    .chain(orbital.auxiliary_entities.iter().flatten())
                {
                    if let EntityRef::Inline(def) = entity_ref {
                        table.insert(
                            def.name.clone(),
                            EntityAccessPolicies {
                                read: def.read_policy.as_ref().map(sexpr_to_value),
                                create: def.create_policy.as_ref().map(sexpr_to_value),
                                update: def.update_policy.as_ref().map(sexpr_to_value),
                                delete: def.delete_policy.as_ref().map(sexpr_to_value),
                                collection: def.collection.clone(),
                                inherited: false,
                                required_create_fields:
                                    crate::entity_access::required_create_field_names(def),
                                required_fields: def
                                    .fields
                                    .iter()
                                    .filter(|f| f.required == Some(true) || f.default.is_some())
                                    .filter_map(|f| f.name.clone())
                                    .collect(),
                                intrinsic_fields: crate::entity_access::intrinsic_field_names(def),
                                derived_fields: crate::entity_access::derived_field_exprs(def),
                            },
                        );
                    }
                }
            }

            // Access is a property of the DATA, so a collection's declared
            // policy governs every entity bound to it. Re-run over the whole
            // accumulated table, not just this schema: an organism's entity and
            // the std-atom entity that shadows it on the same collection arrive
            // in different registrations
            // (`R-SHADOW-ENTITY-SHARES-COLLECTION-DIRECTIVES-DONT-APPLY`).
            crate::entity_access::resolve_collection_inheritance(&mut table);
        }

        // Additive across registrations, same inline-only contract as the access
        // table above. A SEPARATE lock/loop (never nested inside the
        // `entity_policies` write guard above) so `entity_policies` and
        // `entity_definitions` stay two independent leaf locks, not a new
        // nested pair to add to the documented order.
        {
            let mut defs = self.entity_definitions.write();
            for orbital in &schema.orbitals {
                for entity_ref in std::iter::once(&orbital.entity)
                    .chain(orbital.auxiliary_entities.iter().flatten())
                {
                    if let EntityRef::Inline(def) = entity_ref {
                        defs.insert(def.name.clone(), def.clone());
                    }
                }
            }
        }
        {
            let defs = self.entity_definitions.read();
            let mut defaults = self.entity_defaults.write();
            for orbital in &schema.orbitals {
                for entity_ref in std::iter::once(&orbital.entity)
                    .chain(orbital.auxiliary_entities.iter().flatten())
                {
                    if let EntityRef::Inline(def) = entity_ref {
                        if let Some(registered) = defs.get(&def.name) {
                            defaults.insert(
                                def.name.clone(),
                                SharedEntity::from_value(declared_defaults(registered)),
                            );
                        }
                    }
                }
            }
        }

        for orbital in &schema.orbitals {
            let mut registered = Vec::new();
            for trait_ref in &orbital.traits {
                if let Some(trait_def) = self.resolve_trait_definition(trait_ref, &schema) {
                    self.register_trait(&trait_def)?;
                    self.register_trait_listeners(&trait_def, &orbital.name)?;
                    registered.push(trait_def);
                }
            }
            self.capture_children
                .write()
                .extend(super::callsite_capture::capture_children(
                    &registered.iter().collect::<Vec<_>>(),
                ));
        }
        self.bind_program_rows(&schema);

        {
            let mut schemas = self.schemas.write();
            schemas.insert(schema_name.clone(), Arc::new(schema.clone()));
            let mut homes = HashMap::new();
            for (name, schema) in schemas.iter() {
                for (index, orbital) in schema.orbitals.iter().enumerate() {
                    for trait_ref in &orbital.traits {
                        if let Some(trait_name) = trait_ref.trait_name() {
                            homes
                                .entry(trait_name.to_string())
                                .or_insert_with(|| (name.clone(), index));
                        }
                    }
                }
            }
            *self.trait_homes.write() = homes;
        }
        {
            let mut nav = self.nav_pages.write();
            *nav = merge_nav_items(&nav, app_nav_items(&schema));
        }

        // Additive rebuild over the FULL accumulated trait set, not just this
        // schema's — an embed spanning two registrations must still resolve
        // (mirrors the entity-policy table's additive re-run above).
        *self.embed_index.write() = EmbedIndex::build(&self.trait_definitions.read());

        info!(schema = %schema_name, "Schema registered successfully");
        Ok(())
    }

    /// Resolve a trait reference to its definition.
    pub(super) fn resolve_trait_definition(
        &self,
        trait_ref: &TraitReference,
        schema: &OrbitalSchema,
    ) -> Option<TraitDefinition> {
        match trait_ref {
            TraitReference::Inline(def) => Some(def.clone()),
            TraitReference::Reference {
                reference, name, ..
            } => {
                let lookup_name = reference.as_ref().or(name.as_ref())?;
                for orbital in &schema.orbitals {
                    for other_trait_ref in &orbital.traits {
                        if let TraitReference::Inline(trait_def) = other_trait_ref {
                            if &trait_def.name == lookup_name {
                                return Some(trait_def.clone());
                            }
                        }
                    }
                }
                None
            }
        }
    }

    /// Register a trait definition with the state manager.
    /// What a program asks of the session's row store: one table per `persistent:` collection (every entity bound
    /// to it reads and writes the same rows, as `MockPersistenceAdapter` and the TS shell do), the `[runtime]`
    /// entities held in the kernel, and each inline entity's declared instances seeded.
    /// The driver orbital's declared `queryCapabilities`/`writeCapabilities` config (EXEC-STORE-4) are what its
    /// bound entity's push-down honours, the same declaration a host store makes.
    pub(super) fn declare_store_capabilities(&self, schema: &OrbitalSchema, entity_type: &str, driver: &str) {
        let Some(config) = schema
            .orbitals
            .iter()
            .find(|o| o.name == driver)
            .and_then(|o| o.config.as_ref())
        else {
            return;
        };
        let declared = |key: &str| {
            config
                .get(key)
                .and_then(|field| field.default.as_ref())
                .map(|value| serde_json::to_value(value).map_err(|e| e.to_string()))
        };
        if let Some(value) = declared("queryCapabilities") {
            match value.and_then(|v| {
                serde_json::from_value::<QueryCapabilities>(v).map_err(|e| e.to_string())
            }) {
                Ok(capabilities) => self.io.set_query_capabilities(entity_type, capabilities),
                Err(e) => {
                    tracing::error!(driver, entity = entity_type, error = %e, "kernel: store driver queryCapabilities malformed")
                }
            }
        }
        if let Some(value) = declared("writeCapabilities") {
            match value.and_then(|v| {
                serde_json::from_value::<WriteCapabilities>(v).map_err(|e| e.to_string())
            }) {
                Ok(capabilities) => self.io.set_write_capabilities(entity_type, capabilities),
                Err(e) => {
                    tracing::error!(driver, entity = entity_type, error = %e, "kernel: store driver writeCapabilities malformed")
                }
            }
        }
    }

    pub(super) fn bind_program_rows(&self, schema: &OrbitalSchema) {
        for orbital in &schema.orbitals {
            for entity_ref in
                std::iter::once(&orbital.entity).chain(orbital.auxiliary_entities.iter().flatten())
            {
                if let EntityRef::Inline(def) = entity_ref {
                    if let Some(collection) = &def.collection {
                        self.entity_source.bind_collection(&def.name, collection);
                    }
                    if let Some(indexes) = def.indexes.as_deref().filter(|i| !i.is_empty()) {
                        self.entity_source.declare_indexes(&def.name, indexes);
                    }
                    if def.persistence == Some(Persistence::Runtime) {
                        self.io.claim_runtime(&def.name);
                    }
                    if def.persistence != Some(Persistence::Runtime) && !def.browser {
                        if let Some(driver) = def.store.as_ref().or(schema.store.as_ref()) {
                            self.io.bind_store(&def.name, driver);
                            self.declare_store_capabilities(schema, &def.name, driver);
                            let wants = self
                                .io
                                .query_capabilities_of(&def.name)
                                .and_then(|c| c.indexes)
                                .is_some_and(|realization| realization.wants_paths());
                            if let Some(indexes) = def.indexes.as_deref().filter(|i| wants && !i.is_empty()) {
                                if let Some(ensure) = self.io.store_ensure(&def.name, indexes.to_vec()) {
                                    self.store_seeds.lock().push_back(ensure);
                                }
                            }
                        }
                    }
                }
            }
        }
        for orbital in &schema.orbitals {
            if let EntityRef::Inline(entity_def) = &orbital.entity {
                if let Some(instances) = &entity_def.instances {
                    let now = crate::mock_random::iso_from_ms(self.clock_now());
                    let rows = crate::instances::instance_rows(&entity_def.name, instances, &now);
                    if let Some(seed) = self.io.store_seed(&entity_def.name, rows.clone()) {
                        self.store_seeds.lock().push_back(seed);
                        continue;
                    }
                    for entity in rows {
                        let _ = self.entity_source.create(&entity_def.name, &entity);
                    }
                }
            }
        }
    }

    /// A fresh control program with this app's entity metadata, and only the control program's executable routes and grants.
    pub fn program_context(
        &self,
        entity_source: Arc<dyn EntitySource>,
        config: KernelConfig,
        schema: OrbitalSchema,
    ) -> Result<Self, KernelError> {
        let existing = self.schemas.read().clone();
        let definitions = self.entity_definitions.read().clone();
        let traits = self.trait_definitions.read();
        let collision = |kind: &str, name: &str| {
            KernelError::Internal(format!("program context {kind} collision: `{name}`"))
        };
        if existing.contains_key(&schema.name) {
            return Err(collision("schema", &schema.name));
        }
        for orbital in &schema.orbitals {
            if existing
                .values()
                .any(|program| program.orbitals.iter().any(|o| o.name == orbital.name))
            {
                return Err(collision("orbital", &orbital.name));
            }
            for trait_name in orbital.traits.iter().filter_map(|t| t.trait_name()) {
                if traits.contains_key(trait_name) {
                    return Err(collision("trait", trait_name));
                }
            }
            for entity in
                std::iter::once(&orbital.entity).chain(orbital.auxiliary_entities.iter().flatten())
            {
                if let EntityRef::Inline(def) = entity {
                    if definitions.contains_key(&def.name) {
                        return Err(collision("entity", &def.name));
                    }
                    if let Some(collection) = &def.collection {
                        if definitions
                            .values()
                            .any(|old| old.collection.as_ref() == Some(collection))
                        {
                            return Err(collision("collection", collection));
                        }
                    }
                }
            }
        }
        drop(traits);
        let child = Self::new(config.environment, entity_source, config);
        *child.entity_policies.write() = self.entity_policies.read().clone();
        *child.entity_definitions.write() = definitions;
        *child.entity_defaults.write() = self.entity_defaults.read().clone();
        child.set_current_user(self.get_current_user());
        child.register_schema(schema)?;
        Ok(child)
    }

    /// A kernel holding this one's registered program and none of its session: every trait at its initial state,
    /// no frames, rows, deliveries or renders, and the program's declared instances seeded again. A stateless host
    /// registers a program once and serves each request a fork (EXEC-DIST-5).
    pub fn fork_session(
        &self,
        entity_source: Arc<dyn EntitySource>,
        config: KernelConfig,
    ) -> Result<RuntimeKernel, KernelError> {
        let fork = RuntimeKernel::new(config.environment, entity_source, config);
        fork.host_fans_out.store(
            self.host_fans_out.load(std::sync::atomic::Ordering::SeqCst),
            std::sync::atomic::Ordering::SeqCst,
        );
        fork.record_entity_after.store(
            self.record_entity_after
                .load(std::sync::atomic::Ordering::SeqCst),
            std::sync::atomic::Ordering::SeqCst,
        );
        *fork.schemas.write() = self.schemas.read().clone();
        *fork.trait_homes.write() = self.trait_homes.read().clone();
        *fork.trait_definitions.write() = self.trait_definitions.read().clone();
        *fork.entity_policies.write() = self.entity_policies.read().clone();
        *fork.embed_index.write() = self.embed_index.read().clone();
        *fork.entity_definitions.write() = self.entity_definitions.read().clone();
        *fork.entity_defaults.write() = self.entity_defaults.read().clone();
        *fork.egress.write() = self.egress.read().clone();
        *fork.os_grants.write() = self.os_grants.read().clone();
        *fork.theme_presets.write() = self.theme_presets.read().clone();
        *fork.nav_pages.write() = self.nav_pages.read().clone();
        *fork.capture_children.write() = self.capture_children.read().clone();
        *fork.i18n.write() = self.i18n.read().clone();
        *fork.managed_credentials.write() = self.managed_credentials.read().clone();
        *fork.current_user.write() = self.current_user.read().clone();
        fork.deliveries
            .write()
            .copy_placement(&self.deliveries.read());
        let schemas = self.schemas.read();
        let mut names: Vec<&String> = schemas.keys().collect();
        names.sort();
        let mut seen = std::collections::HashSet::new();
        for orbital in names.iter().flat_map(|name| schemas[*name].orbitals.iter()) {
            for trait_name in orbital.traits.iter().filter_map(|t| t.trait_name()) {
                let Some(def) = self.trait_definitions.read().get(trait_name).cloned() else {
                    continue;
                };
                if !seen.insert(trait_name.to_string()) {
                    continue;
                }
                fork.state_manager
                    .write()
                    .register_shared_trait(def.clone());
                fork.register_trait_listeners(&def, &orbital.name)?;
            }
        }
        let homes = self.trait_homes.read();
        let mut rest: Vec<(String, Arc<TraitDefinition>)> = self
            .trait_definitions
            .read()
            .iter()
            .filter(|(name, _)| !seen.contains(name.as_str()))
            .map(|(name, def)| (name.clone(), def.clone()))
            .collect();
        rest.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, def) in rest {
            let orbital = homes
                .get(&name)
                .and_then(|(schema, index)| schemas.get(schema)?.orbitals.get(*index))
                .map(|o| o.name.clone())
                .unwrap_or_default();
            fork.state_manager
                .write()
                .register_shared_trait(def.clone());
            fork.register_trait_listeners(&def, &orbital)?;
        }
        for name in names {
            fork.bind_program_rows(&schemas[name]);
        }
        Ok(fork)
    }

    pub(super) fn register_trait(&self, trait_def: &TraitDefinition) -> Result<(), KernelError> {
        let trait_name = &trait_def.name;
        debug!(trait_name = %trait_name, "Registering trait");

        self.state_manager.write().register_trait(trait_def.clone());
        self.trait_definitions
            .write()
            .insert(trait_name.clone(), Arc::new(trait_def.clone()));

        Ok(())
    }

    /// Register listeners declared on a trait.
    pub(super) fn register_trait_listeners(
        &self,
        trait_def: &TraitDefinition,
        orbital: &str,
    ) -> Result<(), KernelError> {
        if let Some(listens) = &trait_def.listens {
            for listen in listens {
                self.listener_manager
                    .register(&trait_def.name, orbital, listen)
                    .map_err(KernelError::Internal)?;
            }
        }
        Ok(())
    }
}
