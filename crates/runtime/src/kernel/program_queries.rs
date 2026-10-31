//! Reading the registered program: orbitals, pages, mounts, inputs, dispatch modes and trait config.

use super::*;

impl RuntimeKernel {
    /// List all registered orbitals across all schemas.
    pub fn list_orbitals(&self) -> Vec<OrbitalInfo> {
        let schemas = self.registry.schemas.read();
        let mut orbitals = Vec::new();

        for schema in schemas.values() {
            for orbital in &schema.orbitals {
                let entity_name = match &orbital.entity {
                    EntityRef::Inline(e) => e.name.clone(),
                    EntityRef::Reference(r) => r.clone(),
                    EntityRef::Extends(e) => e.reference.clone(),
                };
                orbitals.push(OrbitalInfo {
                    name: orbital.name.clone(),
                    entity_name,
                    trait_count: orbital.traits.len(),
                    page_count: orbital.pages.as_ref().map(|p| p.len()).unwrap_or(0),
                });
            }
        }

        orbitals
    }

    /// Get detailed information for a single orbital.
    pub fn get_orbital(&self, orbital_name: &str) -> Option<OrbitalDetail> {
        let schemas = self.registry.schemas.read();

        for schema in schemas.values() {
            for orbital in &schema.orbitals {
                if orbital.name == orbital_name {
                    return Some(OrbitalDetail {
                        name: orbital.name.clone(),
                        entity: match &orbital.entity {
                            EntityRef::Inline(entity) => EntitySummary {
                                name: entity.name.clone(),
                                fields: entity.fields.clone(),
                            },
                            EntityRef::Reference(reference) => EntitySummary {
                                name: reference.clone(),
                                fields: Vec::new(),
                            },
                            EntityRef::Extends(extends) => EntitySummary {
                                name: extends.reference.clone(),
                                fields: Vec::new(),
                            },
                        },
                        traits: orbital
                            .traits
                            .iter()
                            .filter_map(|t| t.trait_name().map(|s| s.to_string()))
                            .collect(),
                        pages: orbital.pages.clone().unwrap_or_default(),
                    });
                }
            }
        }

        None
    }

    /// Declared external inputs across every registered schema.
    pub fn external_inputs(&self) -> Vec<orbital_core_base::external_inputs::ExternalInput> {
        self.registry.schemas
            .read()
            .values()
            .flat_map(|schema| orbital_core_base::external_inputs::external_inputs_of(schema))
            .collect()
    }

    /// Every inline page of every registered schema, in schema-name order.
    pub(super) fn each_inline_page(&self, mut visit: impl FnMut(&orbital_core_base::schema::types::PageDefinition)) {
        let schemas = self.registry.schemas.read();
        let mut names: Vec<&String> = schemas.keys().collect();
        names.sort();
        for name in names {
            for orbital in &schemas[name].orbitals {
                orbital.pages.iter().flatten().filter_map(|p| p.as_inline()).for_each(&mut visit);
            }
        }
    }

    /// Every trait a route page places, whatever it handles.
    pub(super) fn route_page_traits(&self) -> HashSet<String> {
        let mut traits: HashSet<String> = HashSet::new();
        self.each_inline_page(|page| {
            if page.is_route() {
                traits.extend(page.traits.iter().flatten().filter_map(|t| t.trait_name()).map(str::to_string));
            }
        });
        traits.extend(self.lazy_routed_traits());
        traits
    }

    pub(super) fn mounts_of(&self, keep: impl Fn(&orbital_core_base::schema::types::PageDefinition) -> bool) -> Vec<crate::effects::response::MountSeed> {
        let mut out: Vec<crate::effects::response::MountSeed> = Vec::new();
        self.each_inline_page(|page| {
            if !keep(page) {
                return;
            }
            for trait_name in page.traits.iter().flatten().filter_map(|t| t.trait_name()) {
                if out.iter().any(|seed| seed.r#trait == trait_name) {
                    continue;
                }
                if self.awaits_composer(trait_name) {
                    continue;
                }
                if let Some(event) = self.lifecycle_event_of(trait_name) {
                    out.push(crate::effects::response::MountSeed { r#trait: trait_name.to_string(), event });
                }
            }
        });
        out
    }

    fn mount_state_of(&self, trait_name: &str) -> Option<String> {
        self.get_state(trait_name, None)
            .map(|s| s.current_state)
            .or_else(|| self.get_initial_state(trait_name))
    }

    /// Whether `trait_name` is an embedded child that reads its composer's `@callsitePayload` and only repaints on
    /// mount, and its composer has not composed it yet: the composer's capture pass paints it, as the compiled path
    /// renders it inline, so a mount of its own would paint it without that payload.
    fn awaits_composer(&self, trait_name: &str) -> bool {
        use super::callsite_capture::{config_references_callsite_payload, references_callsite_payload, repaint_lifecycle_event};
        if !self.capture_children.read().values().any(|children| children.iter().any(|child| child == trait_name)) {
            return false;
        }
        if self.composed_callsite.read().contains_key(trait_name) {
            return false;
        }
        let Some(def) = self.registry.trait_definitions.read().get(trait_name).cloned() else {
            return false;
        };
        let reads_payload = references_callsite_payload(&def)
            || serde_json::to_value(config_defaults_value(&def, &self.registry.trait_definitions.read()))
                .ok()
                .is_some_and(|config| config_references_callsite_payload(&config));
        reads_payload
            && self
                .mount_state_of(trait_name)
                .is_some_and(|state| repaint_lifecycle_event(&def, &state).is_some())
    }

    /// The first lifecycle event (`INIT`, `LOAD`, `$MOUNT`) the trait handles from its current state, or its
    /// initial state when it has none yet; `None` when it handles none, so it is not seeded.
    pub(super) fn lifecycle_event_of(&self, trait_name: &str) -> Option<crate::effects::response::LifecycleEvent> {
        use crate::effects::response::LifecycleEvent;
        let def = self.registry.trait_definitions.read().get(trait_name).cloned()?;
        let state = self.mount_state_of(trait_name)?;
        [LifecycleEvent::Init, LifecycleEvent::Load, LifecycleEvent::Mount].into_iter().find(|event| {
            def.state_machine.transitions.iter().any(|t| t.event == event.as_str() && t.from.matches(&state))
        })
    }

    /// What mounts when a host starts (EXEC-ENDPOINT-3): the traits of every endpoint whose
    /// protocol is served by `side`, plus every `boot` endpoint. The client side has no start mounts.
    pub fn boot_mounts(&self, side: orbital_core_base::protocols::ProtocolSide) -> Vec<crate::effects::response::MountSeed> {
        use orbital_core_base::protocols::{protocol_after_preflight, ProtocolSide};
        if side == ProtocolSide::Client {
            return Vec::new();
        }
        self.mounts_of(|page| {
            protocol_after_preflight(page.protocol_name())
                .is_some_and(|spec| spec.side == side || spec.side == ProtocolSide::Start)
        })
    }

    /// What mounts when a client opens `route_path`: the traits of the route pages whose path
    /// matches, and nothing from protocol endpoints.
    pub fn route_mounts(&self, route_path: &str) -> Vec<crate::effects::response::MountSeed> {
        self.mounts_of(|page| {
            page.is_route() && orbital_core_base::route::path_matches_pattern(route_path, &page.path)
        })
    }

    /// The entity a registered trait is bound to.
    pub fn linked_entity_of(&self, trait_name: &str) -> Option<String> {
        self.registry.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
    }

    /// A registered trait's declared dispatch mode (`dispatch_mode::compute`).
    pub fn dispatch_mode_of(&self, trait_def: &TraitDefinition) -> DispatchMode {
        trait_def.dispatch_mode(None, &orbital_core_base::runs_on::Residence::none())
    }

    /// The dispatch mode of the registered trait `trait_name`; `None` when no such trait is registered.
    pub fn trait_dispatch_mode(&self, trait_name: &str) -> Option<DispatchMode> {
        let def = self.registry.trait_definitions.read().get(trait_name).cloned()?;
        Some(self.dispatch_mode_of(&def))
    }

    /// The config a registered trait's steps bind as `@config` (declared defaults, call-site values, `@user`
    /// forwards bound to the session's identity); `None` when no such trait is registered.
    pub fn trait_config(&self, trait_name: &str) -> Result<Option<Value>, KernelError> {
        let user = self.session_viewer()?;
        let defs = self.registry.trait_definitions.read();
        let Some(def) = defs.get(trait_name) else {
            return Ok(None);
        };
        Ok(Some(bind_user_forwards(config_defaults_value(def, &defs), &user)))
    }
}
