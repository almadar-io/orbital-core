//! Registering a program: schema, traits, listeners, stores, credentials, mounts, and the per-request fork.

use super::*;

impl RuntimeKernel {
    /// Traits entering the mount: routed deliveries to them wait for their own lifecycle event.
    pub fn mount_traits<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let awaiting = self.awaiting_lifecycle(traits);
        self.mount.write().awaiting.extend(awaiting);
    }

    /// The traits among `traits` that handle a lifecycle event: only those have one to wait for (a mount seeds
    /// exactly them, `mounts_of`), so a trait with none is ready at once and hears its listens.
    fn awaiting_lifecycle<I, S>(&self, traits: I) -> Vec<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        traits
            .into_iter()
            .map(Into::into)
            .filter(|t: &String| self.lifecycle_event_of(t).is_some())
            .collect()
    }

    /// A new mount replacing the previous one (a page switch): nothing held for
    /// the old mount survives, and `traits` await their lifecycle event.
    pub fn remount_traits<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let awaiting = self.awaiting_lifecycle(traits);
        let mut mount = self.mount.write();
        mount.awaiting.clear();
        mount.held.clear();
        self.renders.write().clear();
        mount.awaiting.extend(awaiting);
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
        let schemas = self.registry.schemas.read();
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
        self.grants.managed_credentials
            .read()
            .iter()
            .cloned()
            .for_each(add);
        Ok(found)
    }

    /// `credential-list`'s raw reply with the credentials the registered programs declare and the installed services registry's catalog beside the store's rows; any
    /// other reply as it is.
    pub(super) fn with_declared(&self, op: orbital_core_base::credential_admin::CredentialAdminOp, reply: Value) -> Result<Value, String> {
        use orbital_core_base::credential_admin::CredentialAdminOp;
        let (CredentialAdminOp::List, Value::Object(mut fields)) = (op, reply.clone()) else {
            return Ok(reply);
        };
        let credentials = self.program_credentials()?;
        let mut names: Vec<String> = Vec::new();
        for credential in &credentials {
            if !names.contains(&credential.env_var) {
                names.push(credential.env_var.clone());
            }
        }
        let held = if names.is_empty() {
            crate::credential_admin::EnvTails::Held { tails: Vec::new() }
        } else {
            self.io.credential_env_tails(&names)?
        };
        let env = match held {
            crate::credential_admin::EnvTails::Held { tails } => tails
                .into_iter()
                .map(|tail| {
                    let mut row = ValueMap::from_iter([("name".into(), Value::String(tail.name.into()))]);
                    if let Some(last4) = tail.last4 {
                        row.insert("last4".into(), Value::String(last4.into()));
                    }
                    Value::object(row)
                })
                .collect::<Vec<_>>(),
            crate::credential_admin::EnvTails::Unsupported => Vec::new(),
        };
        fields.insert("env".into(), Value::array(env));
        let declared = credentials
            .into_iter()
            .map(|credential| {
                Value::object(ValueMap::from_iter([
                    ("group".into(), Value::String(credential.group.into())),
                    ("envVar".into(), Value::String(credential.env_var.into())),
                    ("description".into(), Value::String(credential.description.into())),
                ]))
            })
            .collect::<Vec<_>>();
        fields.insert("declared".into(), Value::array(declared));
        let services = orbital_core_base::language::registry::baked_service_catalog()
            .iter()
            .map(|service| {
                let credentials = service
                    .credentials
                    .iter()
                    .map(|credential| {
                        Value::object(ValueMap::from_iter([
                            ("envVar".into(), Value::String(credential.env_var.clone().into())),
                            ("required".into(), Value::Bool(credential.required)),
                            ("description".into(), Value::String(credential.description.clone().into())),
                        ]))
                    })
                    .collect::<Vec<_>>();
                let mut fields = ValueMap::from_iter([
                    ("name".into(), Value::String(service.name.clone().into())),
                    ("credentials".into(), Value::array(credentials)),
                ]);
                if let Some(backend) = &service.backend {
                    fields.insert("backend".into(), Value::String(backend.clone().into()));
                }
                Value::object(fields)
            })
            .collect::<Vec<_>>();
        fields.insert("services".into(), Value::array(services));
        Ok(Value::object(fields))
    }

    /// Name the program whose credentials this kernel's credential effects manage beside its own (an integrations
    /// console serving the app a host shows): its declared credentials join the catalog, replacing the last one named.
    pub fn set_managed_program(&self, program: &serde_json::Value) {
        *self.grants.managed_credentials.write() =
            orbital_core_base::secrets::program_credentials(program);
    }

    /// The program registered under `name`, as the kernel holds it.
    pub fn registered_schema(&self, name: &str) -> Option<Arc<OrbitalSchema>> {
        self.registry.schemas.read().get(name).cloned()
    }

    /// Every registered program, the lazy ones loaded so far included.
    pub fn registered_programs(&self) -> Vec<Arc<OrbitalSchema>> {
        let schemas = self.registry.schemas.read();
        let mut names: Vec<&String> = schemas.keys().collect();
        names.sort();
        names.into_iter().map(|name| schemas[name].clone()).collect()
    }

    /// The `egress` origins every registered program declares, unioned: what a host's transport may reach.
    pub fn egress(&self) -> Vec<String> {
        self.grants.egress.read().clone()
    }

    /// The `os/` grants every registered program declares, unioned: what a host builds its `OsHost` from.
    pub fn os_grants(&self) -> OsGrants {
        self.grants.os_grants.read().clone()
    }

    /// Register a schema with the kernel.
    pub fn register_schema(&self, schema: OrbitalSchema) -> Result<(), KernelError> {
        self.register_schema_from(schema, "")
    }

    /// [`Self::register_schema`] for a program read from `file` (relative to the booted `.orb`, empty for the booted one): its
    /// lazy orbitals sit beside it.
    pub(super) fn register_schema_from(&self, mut schema: OrbitalSchema, file: &str) -> Result<(), KernelError> {
        if schema.resolved.is_none() && schema.type_contract_version.is_none() {
            return Err(KernelError::UnresolvedProgram { program: schema.name });
        }
        self.registry.row_shapes.write().clear();
        schema.bind_unlinked_traits_to_host_entity();
        if let Some(declared) = &schema.egress {
            let mut egress = self.grants.egress.write();
            for origin in declared {
                if !egress.contains(origin) {
                    egress.push(origin.clone());
                }
            }
        }
        self.grants.os_grants.write().union(&schema.program_os_grants());
        self.register_placement(&schema);
        let schema_name = schema.name.clone();
        info!(schema = %schema_name, "Registering schema");
        self.presentation.theme_presets
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

        // Populate the entity access table from every declared entity's four
        // directives (additive across registrations — a later schema never
        // clobbers an earlier one's policies for an entity it doesn't itself
        // redeclare).
        {
            let mut table = self.registry.entity_policies.write();

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
            let mut defs = self.registry.entity_definitions.write();
            for orbital in &schema.orbitals {
                for entity_ref in std::iter::once(&orbital.entity)
                    .chain(orbital.auxiliary_entities.iter().flatten())
                {
                    if let EntityRef::Inline(def) = entity_ref {
                        defs.insert(def.name.clone(), Arc::new(def.clone()));
                    }
                }
            }
        }
        {
            let defs = self.registry.entity_definitions.read();
            let mut defaults = self.registry.entity_defaults.write();
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

        let boot_lazy = self.register_lazy_stubs(&schema, file)?;
        self.register_stub_listeners(&schema.name, None)?;
        for orbital in &schema.orbitals {
            let mut registered = Vec::new();
            for trait_ref in &orbital.traits {
                if let Some(trait_def) = self.resolve_trait_definition(trait_ref, &schema) {
                    self.register_trait(Arc::clone(&trait_def))?;
                    if !self.lazy.holds_listeners_of(&trait_def.name) {
                        self.register_trait_listeners(&trait_def, &orbital.name)?;
                    }
                    registered.push(trait_def);
                }
            }
            self.capture_children
                .write()
                .extend(super::callsite_capture::capture_children(
                    &registered.iter().map(|t| &**t).collect::<Vec<_>>(),
                ));
            self.register_stub_listeners(&schema.name, Some(&orbital.name))?;
        }
        self.require_stub_listeners_placed(&schema.name)?;
        self.bind_program_rows(&schema);
        let nav_items = app_nav_items(&schema);
        self.resolve_lazy_stubs(&schema);

        {
            let mut schemas = self.registry.schemas.write();
            schemas.insert(schema_name.clone(), Arc::new(schema));
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
            *self.registry.trait_homes.write() = homes;
        }
        {
            let mut nav = self.presentation.nav_pages.write();
            *nav = merge_nav_items(&nav, nav_items);
        }

        // Additive rebuild over the FULL accumulated trait set, not just this
        // schema's — an embed spanning two registrations must still resolve
        // (mirrors the entity-policy table's additive re-run above).
        *self.registry.embed_index.write() = EmbedIndex::build(&self.registry.trait_definitions.read());

        info!(schema = %schema_name, "Schema registered successfully");
        boot_lazy
            .iter()
            .try_for_each(|orbital| self.ensure_orbital_loaded(orbital))?;
        Ok(())
    }

    /// Resolve a trait reference to its definition.
    pub(super) fn resolve_trait_definition(
        &self,
        trait_ref: &TraitReference,
        schema: &OrbitalSchema,
    ) -> Option<Arc<TraitDefinition>> {
        match trait_ref {
            TraitReference::Inline(def) => Some(Arc::clone(def)),
            TraitReference::Reference {
                reference, name, ..
            } => {
                let lookup_name = reference.as_ref().or(name.as_ref())?;
                for orbital in &schema.orbitals {
                    for other_trait_ref in &orbital.traits {
                        if let TraitReference::Inline(trait_def) = other_trait_ref {
                            if &trait_def.name == lookup_name {
                                return Some(Arc::clone(trait_def));
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
        let existing = self.registry.schemas.read().clone();
        let definitions = self.registry.entity_definitions.read().clone();
        let traits = self.registry.trait_definitions.read();
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
        *child.registry.entity_policies.write() = self.registry.entity_policies.read().clone();
        *child.registry.entity_definitions.write() = definitions;
        *child.registry.entity_defaults.write() = self.registry.entity_defaults.read().clone();
        child.register_schema(schema)?;
        Ok(child)
    }

    /// A kernel holding this one's registered program and none of its session: every trait at its initial state,
    /// no frames, rows, deliveries or renders, and the program's declared instances seeded again. A stateless host
    /// registers a program once and serves each request a fork (EXEC-DIST-5). The fork shares the TLS socket table: a
    /// socket is the host's, so a parked operation of a request completes on whichever handle the host holds.
    pub fn fork_session(
        &self,
        entity_source: Arc<dyn EntitySource>,
        config: KernelConfig,
    ) -> Result<RuntimeKernel, KernelError> {
        #[cfg_attr(not(feature = "wire-drivers"), allow(unused_mut))]
        let mut fork = RuntimeKernel::new(config.environment, entity_source, config);
        #[cfg(feature = "wire-drivers")]
        {
            fork.tls = self.tls.clone();
        }
        fork.view_records.keep_none();
        fork.host_fans_out.store(
            self.host_fans_out.load(std::sync::atomic::Ordering::SeqCst),
            std::sync::atomic::Ordering::SeqCst,
        );
        fork.record_entity_after.store(
            self.record_entity_after
                .load(std::sync::atomic::Ordering::SeqCst),
            std::sync::atomic::Ordering::SeqCst,
        );
        *fork.registry.schemas.write() = self.registry.schemas.read().clone();
        *fork.registry.trait_homes.write() = self.registry.trait_homes.read().clone();
        *fork.registry.trait_definitions.write() = self.registry.trait_definitions.read().clone();
        *fork.registry.sealed_emit_fields.write() = self.registry.sealed_emit_fields.read().clone();
        *fork.registry.entity_policies.write() = self.registry.entity_policies.read().clone();
        *fork.registry.embed_index.write() = self.registry.embed_index.read().clone();
        *fork.registry.entity_definitions.write() = self.registry.entity_definitions.read().clone();
        *fork.registry.entity_defaults.write() = self.registry.entity_defaults.read().clone();
        *fork.grants.egress.write() = self.grants.egress.read().clone();
        *fork.grants.os_grants.write() = self.grants.os_grants.read().clone();
        *fork.presentation.theme_presets.write() = self.presentation.theme_presets.read().clone();
        *fork.presentation.nav_pages.write() = self.presentation.nav_pages.read().clone();
        *fork.capture_children.write() = self.capture_children.read().clone();
        *fork.i18n.write() = self.i18n.read().clone();
        *fork.mount_ledger.write() = self.mount_ledger.read().clone();
        *fork.grants.managed_credentials.write() = self.grants.managed_credentials.read().clone();
        *fork.sessions.identities.write() = self.sessions.identities.read().clone();
        fork.deliveries
            .write()
            .copy_placement(&self.deliveries.read());
        let schemas = self.registry.schemas.read();
        let mut names: Vec<&String> = schemas.keys().collect();
        names.sort();
        let mut seen = std::collections::HashSet::new();
        for orbital in names.iter().flat_map(|name| schemas[*name].orbitals.iter()) {
            for trait_name in orbital.traits.iter().filter_map(|t| t.trait_name()) {
                let Some(def) = self.registry.trait_definitions.read().get(trait_name).cloned() else {
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
        let homes = self.registry.trait_homes.read();
        let mut rest: Vec<(String, Arc<TraitDefinition>)> = self
            .registry.trait_definitions
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
        fork.inherit_lazy_stubs(self)?;
        Ok(fork)
    }

    /// One definition per trait, shared by the state manager and the registry.
    pub(super) fn register_trait(&self, trait_def: Arc<TraitDefinition>) -> Result<(), KernelError> {
        debug!(trait_name = %trait_def.name, "Registering trait");
        self.state_manager.write().register_shared_trait(Arc::clone(&trait_def));
        self.register_sealed_emit_fields(&trait_def);
        self.registry.trait_definitions.write().insert(trait_def.name.clone(), trait_def);
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
