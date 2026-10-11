//! OIR Validation
//!
//! Validates OIR modules for correctness before code generation.

use std::collections::HashSet;

use super::{
    OirBinding, OirBindingRoot, OirEffect, OirEntity, OirExpr, OirModule, OirPage, OirPersistence,
    OirTrait, OirTransition, OirTransitionFrom,
};

/// Validation error
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
    pub severity: ValidationSeverity,
}

impl ValidationError {
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            severity: ValidationSeverity::Error,
        }
    }

    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            severity: ValidationSeverity::Warning,
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.severity, self.path, self.message)
    }
}

/// Validation severity
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationSeverity {
    Warning,
    Error,
}

impl std::fmt::Display for ValidationSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warning => write!(f, "WARNING"),
            Self::Error => write!(f, "ERROR"),
        }
    }
}

/// Validation result
#[derive(Debug, Clone, Default)]
pub struct ValidationResult {
    pub errors: Vec<ValidationError>,
}

impl ValidationResult {
    pub fn new() -> Self {
        Self { errors: vec![] }
    }

    pub fn add_error(&mut self, error: ValidationError) {
        self.errors.push(error);
    }

    pub fn merge(&mut self, other: ValidationResult) {
        self.errors.extend(other.errors);
    }

    pub fn is_valid(&self) -> bool {
        !self
            .errors
            .iter()
            .any(|e| e.severity == ValidationSeverity::Error)
    }

    pub fn has_warnings(&self) -> bool {
        self.errors
            .iter()
            .any(|e| e.severity == ValidationSeverity::Warning)
    }

    pub fn error_count(&self) -> usize {
        self.errors
            .iter()
            .filter(|e| e.severity == ValidationSeverity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.errors
            .iter()
            .filter(|e| e.severity == ValidationSeverity::Warning)
            .count()
    }
}

/// OIR Validator
pub struct OirValidator<'a> {
    module: &'a OirModule,
    entity_names: HashSet<String>,
    trait_names: HashSet<String>,
}

impl<'a> OirValidator<'a> {
    /// Create a new validator for a module
    pub fn new(module: &'a OirModule) -> Self {
        // Declared entities PLUS the ones this module only expects — a
        // relation/linkedEntity/fetch naming an expected entity is legitimate
        // here; the provider is checked at the link step once a composing
        // program supplies it.
        let mut entity_names: HashSet<_> = module.entities.iter().map(|e| e.name.clone()).collect();
        entity_names.extend(module.expected_entities.iter().cloned());
        let trait_names: HashSet<_> = module.traits.iter().map(|t| t.name.clone()).collect();

        Self {
            module,
            entity_names,
            trait_names,
        }
    }

    /// Validate the entire module
    pub fn validate(&self) -> ValidationResult {
        let mut result = ValidationResult::new();

        // Validate module-level
        self.validate_module(&mut result);

        // Validate entities
        for entity in &self.module.entities {
            self.validate_entity(entity, &mut result);
        }

        // Validate traits
        for trait_def in &self.module.traits {
            self.validate_trait(trait_def, &mut result);
        }

        // Validate pages
        for page in &self.module.pages {
            self.validate_page(page, &mut result);
        }

        result
    }

    fn validate_module(&self, result: &mut ValidationResult) {
        // Check module name
        if self.module.name.is_empty() {
            result.add_error(ValidationError::error(
                "module",
                "Module name cannot be empty",
            ));
        }

        // Check version format (simple check)
        if self.module.version.is_empty() {
            result.add_error(ValidationError::error(
                "module",
                "Module version cannot be empty",
            ));
        }

        // Check for duplicate entity names. `[runtime]` entities are exempt:
        // they're per-orbital singletons (no real persistence collection), so
        // when an upstream chrome trait carrying a runtime entity is composed
        // into multiple consumer orbitals the cloned entities are nominal-only
        // collisions, not actual data conflicts.
        //
        // Persistent entities are conditionally exempt: when two orbitals
        // import the SAME atom (e.g. std-technician-dispatch), both bring an
        // identical clone of the atom's persistent entity (`Technician`) as
        // an aux-entity import. The duplication is intentional (both
        // orbitals reference the same backing collection); errors only fire
        // when the duplicates have DIFFERENT definitions (genuine name
        // collision between distinct entities).
        use std::collections::HashMap;
        let mut by_name: HashMap<&str, Vec<&crate::ir::types::OirEntity>> = HashMap::new();
        for entity in &self.module.entities {
            if matches!(entity.persistence, OirPersistence::Runtime) {
                continue;
            }
            by_name
                .entry(entity.name.as_str())
                .or_default()
                .push(entity);
        }
        for (name, group) in by_name {
            if group.len() <= 1 {
                continue;
            }
            // All identical (same atom imported into multiple orbitals)? Then
            // pass — the dedupe is a no-op at codegen time. Compare by
            // collection + persistence + field-name set (full field-by-field
            // structural equality would be brittle if a downstream pass
            // adds metadata).
            let first = group[0];
            let first_fields: std::collections::BTreeSet<&str> =
                first.fields.iter().map(|f| f.name.as_str()).collect();
            let all_identical = group.iter().all(|e| {
                e.collection == first.collection
                    && e.persistence == first.persistence
                    && e.fields
                        .iter()
                        .map(|f| f.name.as_str())
                        .collect::<std::collections::BTreeSet<&str>>()
                        == first_fields
            });
            if all_identical {
                continue;
            }
            result.add_error(ValidationError::error(
                format!("entities.{}", name),
                format!("Duplicate entity name: '{}'. Multiple orbitals declare an entity with this name but DIFFERENT definitions (collection / persistence / field set differ). Each entity name must be unique across all orbitals unless it's the same atom imported into multiple orbitals.", name),
            ));
        }
        let mut seen: HashSet<&String> = HashSet::new();

        // Check for duplicate trait names
        seen.clear();
        for trait_def in &self.module.traits {
            if !seen.insert(&trait_def.name) {
                result.add_error(ValidationError::error(
                    format!("traits.{}", trait_def.name),
                    format!("Duplicate trait name: '{}'. Each trait must have a unique name. Consider prefixing with entity name (e.g., '{}Management').", trait_def.name, trait_def.name),
                ));
            }
        }

        // Check for duplicate page names
        seen.clear();
        for page in &self.module.pages {
            if !seen.insert(&page.name) {
                result.add_error(ValidationError::error(
                    format!("pages.{}", page.name),
                    format!(
                        "Duplicate page name: '{}'. Each page must have a unique name.",
                        page.name
                    ),
                ));
            }
        }
        // Duplicate page PATH is the compiler phase's job now
        // (`validation::page::validate_page_path_uniqueness`, param-shape
        // normalized) — this OIR layer only caught the exact-string case,
        // which is a strict subset.
    }

    fn validate_entity(&self, entity: &OirEntity, result: &mut ValidationResult) {
        let path = format!("entities.{}", entity.name);

        // Check entity name
        if entity.name.is_empty() {
            result.add_error(ValidationError::error(&path, "Entity name cannot be empty"));
        }

        // Check for duplicate field names
        let mut field_names = HashSet::new();
        for field in &entity.fields {
            if !field_names.insert(&field.name) {
                result.add_error(ValidationError::error(
                    format!("{}.fields.{}", path, field.name),
                    "Duplicate field name",
                ));
            }
        }

        // Validate relations
        for field in &entity.fields {
            if let Some(relation) = &field.relation {
                if !self.entity_names.contains(&relation.target_entity) {
                    result.add_error(ValidationError::error(
                        format!("{}.fields.{}", path, field.name),
                        format!(
                            "Relation target entity '{}' does not exist",
                            relation.target_entity
                        ),
                    ));
                }
            }
        }
    }

    fn validate_trait(&self, trait_def: &OirTrait, result: &mut ValidationResult) {
        let path = format!("traits.{}", trait_def.name);

        // Check trait name
        if trait_def.name.is_empty() {
            result.add_error(ValidationError::error(&path, "Trait name cannot be empty"));
        }

        // Check linked entity exists
        if let Some(linked) = &trait_def.linked_entity {
            if !self.entity_names.contains(linked) {
                result.add_error(ValidationError::error(
                    &path,
                    format!("Linked entity '{}' does not exist", linked),
                ));
            }
        }

        // Validate state machine
        self.validate_state_machine(trait_def, result);

        // Validate transitions
        for (i, transition) in trait_def.transitions.iter().enumerate() {
            self.validate_transition(trait_def, transition, i, result);
        }

        // Validate listeners
        for listener in &trait_def.listeners {
            // Cross-orbital listeners reference a sibling orbital's trait — that
            // trait isn't in `self.trait_names` (which is scoped to the orbital
            // being validated). Skip the existence check; cross-orbital
            // resolution is handled by `validate_listens_integrity` after all
            // orbitals are loaded.
            if listener.source_orbital.is_some() {
                continue;
            }
            if let Some(source) = &listener.source_trait {
                if !self.trait_names.contains(source) {
                    result.add_error(ValidationError::error(
                        format!("{}.listeners", path),
                        format!("Source trait '{}' does not exist", source),
                    ));
                }
            }
        }
    }

    fn validate_state_machine(&self, trait_def: &OirTrait, result: &mut ValidationResult) {
        let path = format!("traits.{}", trait_def.name);

        // Check for at least one state
        if trait_def.states.is_empty() {
            result.add_error(ValidationError::error(
                &path,
                "Trait must have at least one state",
            ));
            return;
        }

        // Check for exactly one initial state
        let initial_count = trait_def.states.iter().filter(|s| s.is_initial).count();
        if initial_count == 0 {
            result.add_error(ValidationError::error(
                &path,
                "Trait must have an initial state",
            ));
        } else if initial_count > 1 {
            result.add_error(ValidationError::error(
                &path,
                "Trait cannot have multiple initial states",
            ));
        }

        // Check for duplicate state names
        let mut state_names = HashSet::new();
        for state in &trait_def.states {
            if !state_names.insert(&state.name) {
                result.add_error(ValidationError::error(
                    format!("{}.states.{}", path, state.name),
                    "Duplicate state name",
                ));
            }
        }
    }

    fn validate_transition(
        &self,
        trait_def: &OirTrait,
        transition: &OirTransition,
        index: usize,
        result: &mut ValidationResult,
    ) {
        let path = format!("traits.{}.transitions[{}]", trait_def.name, index);
        let state_names: HashSet<_> = trait_def.states.iter().map(|s| &s.name).collect();

        // Check source state exists
        match &transition.from {
            OirTransitionFrom::Single(state) => {
                if !state_names.contains(state) {
                    result.add_error(ValidationError::error(
                        &path,
                        format!("Source state '{}' does not exist", state),
                    ));
                }
            }
            OirTransitionFrom::Multiple(states) => {
                for state in states {
                    if !state_names.contains(state) {
                        result.add_error(ValidationError::error(
                            &path,
                            format!("Source state '{}' does not exist", state),
                        ));
                    }
                }
            }
            OirTransitionFrom::Any => {}
        }

        // Check target state exists
        if !state_names.contains(&transition.to) {
            result.add_error(ValidationError::error(
                &path,
                format!("Target state '{}' does not exist", transition.to),
            ));
        }

        // Check event exists
        let event_keys: HashSet<_> = trait_def.events.iter().map(|e| &e.key).collect();
        if !event_keys.contains(&transition.event) {
            result.add_error(ValidationError::error(
                &path,
                format!("Event '{}' is not defined", transition.event),
            ));
        }

        // Validate guard expression
        if let Some(guard) = &transition.guard {
            self.validate_expression(guard, &path, "guard", result);
        }

        // Validate effect expressions
        for (i, effect) in transition.effects.iter().enumerate() {
            self.validate_expression(effect, &path, &format!("effects[{}]", i), result);
        }
    }

    fn validate_expression(
        &self,
        expr: &OirExpr,
        path: &str,
        context: &str,
        result: &mut ValidationResult,
    ) {
        // Validate bindings reference valid entities
        for binding in expr.collect_bindings() {
            self.validate_binding(binding, path, context, result);
        }

        // Validate nested effects
        if let OirExpr::Effect { effect } = expr {
            self.validate_effect(effect, path, context, result);
        }
    }

    fn validate_binding(
        &self,
        binding: &OirBinding,
        _path: &str,
        _context: &str,
        _result: &mut ValidationResult,
    ) {
        // We can't fully validate bindings without knowing the runtime context,
        // but we can check for obviously invalid references
        match &binding.root {
            OirBindingRoot::Entity => {
                // Entity bindings are valid if we have a linked entity
            }
            OirBindingRoot::Payload => {
                // Payload bindings depend on event definition
            }
            OirBindingRoot::State => {
                // State binding is always valid
            }
            OirBindingRoot::Config => {
                // Config binding depends on trait config schema
            }
            OirBindingRoot::User => {
                // Always valid (runtime resolved)
            }
            OirBindingRoot::Trait(_) => {
                // Client-only binding; deeper validation (trait existence,
                // cycles, self-reference) runs in the binding-validator pass
                // in `orbital-compiler` where the full schema is available.
            }
            OirBindingRoot::Event
            | OirBindingRoot::PrevEvents
            | OirBindingRoot::PrevStates
            | OirBindingRoot::FromState
            | OirBindingRoot::ToState => {
                // Dispatch roots; the L2 validator rejects them in ticks and checks `@event` paths.
            }
            OirBindingRoot::Locale => {
                // Render-time; `orb validate` keeps it inside `render-ui`.
            }
            OirBindingRoot::Pages | OirBindingRoot::CurrentTheme => {
                // Render-resolved schema sigils; valid only as config
                // defaults. Structural enforcement (config-default-only,
                // resolution against the loaded OrbitalSchema) runs in
                // `orbital-compiler` where the full schema is available.
            }
            OirBindingRoot::Local(_) => {
                // Local bindings are validated during evaluation
            }
        }
    }

    fn validate_effect(
        &self,
        effect: &OirEffect,
        path: &str,
        context: &str,
        result: &mut ValidationResult,
    ) {
        match effect {
            OirEffect::Fetch { entity_type, .. } => {
                if !self.entity_names.contains(entity_type) {
                    result.add_error(ValidationError::error(
                        format!("{}.{}", path, context),
                        format!("Fetch entity '{}' does not exist", entity_type),
                    ));
                }
            }
            OirEffect::Persist { entity_type, .. } => {
                if !self.entity_names.contains(entity_type) {
                    result.add_error(ValidationError::error(
                        format!("{}.{}", path, context),
                        format!("Persist entity '{}' does not exist", entity_type),
                    ));
                }
            }
            OirEffect::PersistBatch { entities, .. } => {
                for entity_type in entities {
                    if !self.entity_names.contains(entity_type) {
                        result.add_error(ValidationError::error(
                            format!("{}.{}", path, context),
                            format!("Persist batch entity '{}' does not exist", entity_type),
                        ));
                    }
                }
            }
            OirEffect::Spawn { entity_type, .. } => {
                if !self.entity_names.contains(entity_type) {
                    result.add_error(ValidationError::error(
                        format!("{}.{}", path, context),
                        format!(
                            "Spawn entity '{}' does not exist (may be game entity)",
                            entity_type
                        ),
                    ));
                }
            }
            _ => {}
        }
    }

    fn validate_page(&self, page: &OirPage, result: &mut ValidationResult) {
        let path = format!("pages.{}", page.name);

        // Check page name
        if page.name.is_empty() {
            result.add_error(ValidationError::error(&path, "Page name cannot be empty"));
        }

        // Check page path
        if page.path.is_empty() {
            result.add_error(ValidationError::error(&path, "Page path cannot be empty"));
        }

        // Validate trait bindings
        for binding in &page.traits {
            if !self.trait_names.contains(&binding.trait_name) {
                result.add_error(ValidationError::error(
                    format!("{}.traits", path),
                    format!("Trait '{}' does not exist", binding.trait_name),
                ));
            }
        }

        // Validate entity bindings
        for binding in &page.entity_bindings {
            if !self.entity_names.contains(&binding.entity_name) {
                result.add_error(ValidationError::error(
                    format!("{}.entities", path),
                    format!("Entity '{}' does not exist", binding.entity_name),
                ));
            }
        }

        // Validate sections
        let mut section_ids = HashSet::new();
        for section in &page.sections {
            if !section_ids.insert(&section.id) {
                result.add_error(ValidationError::error(
                    format!("{}.sections", path),
                    format!("Duplicate section id: {}", section.id),
                ));
            }
        }
    }
}

/// Validate an OIR module
pub fn validate(module: &OirModule) -> ValidationResult {
    OirValidator::new(module).validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{OirEvent, OirField, OirState, OirType};

    fn create_test_module() -> OirModule {
        let mut module = OirModule::new("TestApp", "1.0.0");

        // Add entity
        let mut entity = OirEntity::new("Task");
        entity.fields.push(OirField::new("title", OirType::String));
        module.entities.push(entity);

        // Add trait with state machine
        let mut trait_def = OirTrait::new("TaskLifecycle");
        trait_def.linked_entity = Some("Task".to_string());
        trait_def.states.push(OirState::initial("idle"));
        trait_def.states.push(OirState::new("running"));
        trait_def.events.push(OirEvent::new("START"));
        trait_def
            .transitions
            .push(OirTransition::new("idle", "running", "START"));
        module.traits.push(trait_def);

        // Add page
        let mut page = OirPage::new("TaskList", "/tasks");
        page.traits
            .push(super::super::OirTraitBinding::new("TaskLifecycle"));
        module.pages.push(page);

        module
    }

    #[test]
    fn test_valid_module() {
        let module = create_test_module();
        let result = validate(&module);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_duplicate_entity() {
        let mut module = create_test_module();
        module.entities.push(OirEntity::new("Task")); // Duplicate

        let result = validate(&module);
        assert!(!result.is_valid());
        assert!(result
            .errors
            .iter()
            .any(|e| e.message.contains("Duplicate entity")));
    }

    #[test]
    fn test_missing_initial_state() {
        let mut module = create_test_module();
        module.traits[0].states[0].is_initial = false;

        let result = validate(&module);
        assert!(!result.is_valid());
        assert!(result
            .errors
            .iter()
            .any(|e| e.message.contains("initial state")));
    }

    #[test]
    fn test_invalid_transition_target() {
        let mut module = create_test_module();
        module.traits[0].transitions[0].to = "nonexistent".to_string();

        let result = validate(&module);
        assert!(!result.is_valid());
        assert!(result
            .errors
            .iter()
            .any(|e| e.message.contains("does not exist")));
    }

    #[test]
    fn test_invalid_linked_entity() {
        let mut module = create_test_module();
        module.traits[0].linked_entity = Some("Nonexistent".to_string());

        let result = validate(&module);
        assert!(!result.is_valid());
        assert!(result
            .errors
            .iter()
            .any(|e| e.message.contains("Linked entity")));
    }
}
