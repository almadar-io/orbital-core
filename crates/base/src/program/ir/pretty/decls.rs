//! Writing a module's declarations: entities, fields, types, traits, states, transitions and pages.

use super::*;

impl PrettyPrinter {
    pub(super) fn write_module(&mut self, module: &OirModule) {
        self.writeln(&format!("Module: {} v{}", module.name, module.version));

        if self.config.show_descriptions {
            if let Some(desc) = &module.description {
                self.writeln(&format!("  {}", desc));
            }
        }

        self.writeln("");

        // Entities
        if !module.entities.is_empty() {
            self.writeln(&format!("Entities ({}):", module.entities.len()));
            self.depth += 1;
            for entity in &module.entities {
                self.write_entity(entity);
            }
            self.depth -= 1;
            self.writeln("");
        }

        // Traits
        if !module.traits.is_empty() {
            self.writeln(&format!("Traits ({}):", module.traits.len()));
            self.depth += 1;
            for trait_def in &module.traits {
                self.write_trait(trait_def);
            }
            self.depth -= 1;
            self.writeln("");
        }

        // Pages
        if !module.pages.is_empty() {
            self.writeln(&format!("Pages ({}):", module.pages.len()));
            self.depth += 1;
            for page in &module.pages {
                self.write_page(page);
            }
            self.depth -= 1;
        }

        if self.config.show_metadata {
            self.writeln("");
            self.writeln("Metadata:");
            self.depth += 1;
            if let Some(at) = &module.metadata.generated_at {
                self.writeln(&format!("Generated: {}", at));
            }
            if let Some(ver) = &module.metadata.compiler_version {
                self.writeln(&format!("Compiler: {}", ver));
            }
            self.depth -= 1;
        }
    }

    pub(super) fn write_entity(&mut self, entity: &OirEntity) {
        let persistence = match entity.persistence {
            OirPersistence::Persistent => "",
            OirPersistence::Runtime => " (runtime)",
        };

        self.writeln(&format!("entity {}{}", entity.name, persistence));

        if self.config.show_descriptions {
            if let Some(desc) = &entity.description {
                self.depth += 1;
                self.writeln(&format!("# {}", desc));
                self.depth -= 1;
            }
        }

        self.depth += 1;
        for field in &entity.fields {
            self.write_field(field);
        }
        self.depth -= 1;
    }

    pub(super) fn write_field(&mut self, field: &OirField) {
        let required = if field.required { "" } else { "?" };
        let type_str = self.format_type(&field.field_type);
        let intrinsic = if field.intrinsic { " (intrinsic)" } else { "" };
        self.writeln(&format!(
            "{}{}: {}{}",
            field.name, required, type_str, intrinsic
        ));
    }

    pub(super) fn format_type(&self, ty: &OirType) -> String {
        match ty {
            OirType::String => "String".to_string(),
            OirType::Number => "Number".to_string(),
            OirType::Integer => "Integer".to_string(),
            OirType::Boolean => "Boolean".to_string(),
            OirType::Date => "Date".to_string(),
            OirType::DateTime => "DateTime".to_string(),
            OirType::Instant => "Instant".to_string(),
            OirType::Email => "Email".to_string(),
            OirType::Url => "Url".to_string(),
            OirType::Phone => "Phone".to_string(),
            OirType::Uuid => "Uuid".to_string(),
            OirType::Image => "Image".to_string(),
            OirType::Bytes => "Bytes".to_string(),
            OirType::CallKey => "CallKey".to_string(),
            OirType::FieldRef { record, value: Some(value) } => {
                format!("Field<{}, {}>", self.format_type(record), self.format_type(value))
            }
            OirType::FieldRef { record, value: None } => format!("Field<{}>", self.format_type(record)),
            OirType::Money => "Money".to_string(),
            OirType::File => "File".to_string(),
            OirType::Node => "Node".to_string(),
            OirType::Array { inner } => format!("[{}]", self.format_type(inner)),
            OirType::Tuple { items } => format!(
                "[{}]",
                items
                    .iter()
                    .map(|t| self.format_type(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            OirType::Map { key, value } => {
                format!(
                    "Map<{}, {}>",
                    self.format_type(key),
                    self.format_type(value)
                )
            }
            OirType::Optional { inner } => format!("{}?", self.format_type(inner)),
            OirType::Reference { entity } => format!("-> {}", entity),
            OirType::Row { entity } => entity.clone(),
            OirType::Enum { name } => format!("enum {}", name),
            OirType::Struct { fields, .. } => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, self.format_type(&f.field_type)))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            OirType::Union { variants } => {
                let parts: Vec<String> = variants
                    .iter()
                    .map(|v| format!("{}: {}", v.name, self.format_type(&v.field_type)))
                    .collect();
                format!("({})", parts.join(" | "))
            }
            OirType::Trait => "Trait".to_string(),
            OirType::Orbital => "Orbital".to_string(),
            OirType::Behavior => "Behavior".to_string(),
            OirType::SExpr => "SExpr".to_string(),
            OirType::TypeRef { name } => format!("&{}", name),
            OirType::Var { name } => name.to_string(),
        }
    }

    pub(super) fn write_trait(&mut self, trait_def: &OirTrait) {
        let linked = trait_def
            .linked_entity
            .as_ref()
            .map(|e| format!(" for {}", e))
            .unwrap_or_default();

        self.writeln(&format!("trait {}{}", trait_def.name, linked));

        if self.config.show_descriptions {
            if let Some(desc) = &trait_def.description {
                self.depth += 1;
                self.writeln(&format!("# {}", desc));
                self.depth -= 1;
            }
        }

        self.depth += 1;

        // States
        if !trait_def.states.is_empty() {
            self.writeln("states:");
            self.depth += 1;
            for state in &trait_def.states {
                self.write_state(state);
            }
            self.depth -= 1;
        }

        // Events
        if !trait_def.events.is_empty() {
            self.writeln("events:");
            self.depth += 1;
            for event in &trait_def.events {
                self.writeln(&event.key);
            }
            self.depth -= 1;
        }

        // Transitions
        if !trait_def.transitions.is_empty() {
            self.writeln("transitions:");
            self.depth += 1;
            for transition in &trait_def.transitions {
                self.write_transition(transition);
            }
            self.depth -= 1;
        }

        self.depth -= 1;
    }

    pub(super) fn write_state(&mut self, state: &OirState) {
        let mut modifiers = vec![];
        if state.is_initial {
            modifiers.push("initial");
        }
        if state.is_final {
            modifiers.push("final");
        }

        let mods = if modifiers.is_empty() {
            String::new()
        } else {
            format!(" ({})", modifiers.join(", "))
        };

        self.writeln(&format!("{}{}", state.name, mods));
    }

    pub(super) fn write_transition(&mut self, transition: &OirTransition) {
        let from = match &transition.from {
            OirTransitionFrom::Single(s) => s.clone(),
            OirTransitionFrom::Multiple(states) => format!("[{}]", states.join(", ")),
            OirTransitionFrom::Any => "*".to_string(),
        };

        self.writeln(&format!(
            "{} --[{}]--> {}",
            from, transition.event, transition.to
        ));

        if transition.guard.is_some() || !transition.effects.is_empty() {
            self.depth += 1;

            if let Some(guard) = &transition.guard {
                self.write("guard: ");
                self.write_expr(guard);
                self.write("\n");
            }

            for effect in &transition.effects {
                self.write("effect: ");
                self.write_expr(effect);
                self.write("\n");
            }

            self.depth -= 1;
        }
    }

    pub(super) fn write_page(&mut self, page: &OirPage) {
        self.writeln(&format!("page {} -> {}", page.name, page.path));

        self.depth += 1;

        // Traits
        if !page.traits.is_empty() {
            let traits: Vec<_> = page.traits.iter().map(|t| t.trait_name.as_str()).collect();
            self.writeln(&format!("uses: {}", traits.join(", ")));
        }

        // Sections
        if !page.sections.is_empty() {
            self.writeln("sections:");
            self.depth += 1;
            for section in &page.sections {
                self.writeln(&format!("{}: {}", section.id, section.pattern.pattern_type));
            }
            self.depth -= 1;
        }

        self.depth -= 1;
    }
}
