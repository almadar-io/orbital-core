//! OIR Pretty Printer
//!
//! Human-readable formatting for OIR structures.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::fmt::Write;

use crate::ir::effects::OirEffect;
use crate::ir::expr::{OirBinaryOp, OirBinding, OirBindingRoot, OirExpr, OirUnaryOp};
use crate::ir::types::{
    OirEntity, OirField, OirModule, OirPage, OirPersistence, OirState, OirTrait, OirTransition,
    OirTransitionFrom, OirType,
};

/// Pretty print configuration
#[derive(Debug, Clone)]
pub struct PrettyConfig {
    /// Indentation string (default: 2 spaces)
    pub indent: String,
    /// Show descriptions
    pub show_descriptions: bool,
    /// Show metadata
    pub show_metadata: bool,
    /// Maximum items before truncating
    pub max_items: Option<usize>,
}

impl Default for PrettyConfig {
    fn default() -> Self {
        Self {
            indent: "  ".to_string(),
            show_descriptions: true,
            show_metadata: false,
            max_items: None,
        }
    }
}

/// Pretty printer for OIR
pub struct PrettyPrinter {
    config: PrettyConfig,
    output: String,
    depth: usize,
}

impl PrettyPrinter {
    /// Create a new pretty printer
    pub fn new() -> Self {
        Self::with_config(PrettyConfig::default())
    }

    /// Create with custom config
    pub fn with_config(config: PrettyConfig) -> Self {
        Self {
            config,
            output: String::new(),
            depth: 0,
        }
    }

    /// Print a module
    pub fn print_module(mut self, module: &OirModule) -> String {
        self.write_module(module);
        self.output
    }

    /// Print an entity
    pub fn print_entity(mut self, entity: &OirEntity) -> String {
        self.write_entity(entity);
        self.output
    }

    /// Print a trait
    pub fn print_trait(mut self, trait_def: &OirTrait) -> String {
        self.write_trait(trait_def);
        self.output
    }

    /// Print a page
    pub fn print_page(mut self, page: &OirPage) -> String {
        self.write_page(page);
        self.output
    }

    /// Print an expression
    pub fn print_expr(mut self, expr: &OirExpr) -> String {
        self.write_expr(expr);
        self.output
    }

    // Internal helpers

    fn indent(&self) -> String {
        self.config.indent.repeat(self.depth)
    }

    fn writeln(&mut self, s: &str) {
        writeln!(self.output, "{}{}", self.indent(), s).unwrap();
    }

    fn write(&mut self, s: &str) {
        write!(self.output, "{}", s).unwrap();
    }

    fn write_module(&mut self, module: &OirModule) {
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

    fn write_entity(&mut self, entity: &OirEntity) {
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

    fn write_field(&mut self, field: &OirField) {
        let required = if field.required { "" } else { "?" };
        let type_str = self.format_type(&field.field_type);
        let intrinsic = if field.intrinsic { " (intrinsic)" } else { "" };
        self.writeln(&format!(
            "{}{}: {}{}",
            field.name, required, type_str, intrinsic
        ));
    }

    fn format_type(&self, ty: &OirType) -> String {
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
            OirType::Json => "Json".to_string(),
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
            OirType::Var { name } => name.clone(),
        }
    }

    fn write_trait(&mut self, trait_def: &OirTrait) {
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

    fn write_state(&mut self, state: &OirState) {
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

    fn write_transition(&mut self, transition: &OirTransition) {
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

    fn write_page(&mut self, page: &OirPage) {
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

    fn write_expr(&mut self, expr: &OirExpr) {
        match expr {
            OirExpr::Null => self.write("null"),
            OirExpr::Bool { value } => self.write(&value.to_string()),
            OirExpr::Number { value } => self.write(&value.to_string()),
            OirExpr::String { value } => self.write(&format!("\"{}\"", value)),
            OirExpr::Array { items } => {
                self.write("[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write_expr(item);
                }
                self.write("]");
            }
            OirExpr::Object { fields } => {
                self.write("{ ");
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write(&format!("{}: ", k));
                    self.write_expr(v);
                }
                self.write(" }");
            }
            OirExpr::Binding { binding } => {
                self.write_binding(binding);
            }
            OirExpr::BinaryOp { op, left, right } => {
                self.write("(");
                self.write_expr(left);
                self.write(&format!(" {} ", self.format_binary_op(op)));
                self.write_expr(right);
                self.write(")");
            }
            OirExpr::UnaryOp { op, operand } => {
                self.write(&self.format_unary_op(op));
                self.write_expr(operand);
            }
            OirExpr::Conditional {
                condition,
                then_branch,
                else_branch,
            } => {
                self.write("if ");
                self.write_expr(condition);
                self.write(" then ");
                self.write_expr(then_branch);
                self.write(" else ");
                self.write_expr(else_branch);
            }
            OirExpr::Call { function, args } => {
                self.write(&format!("{}(", function));
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write_expr(arg);
                }
                self.write(")");
            }
            OirExpr::Lambda { params, body } => {
                self.write(&format!("({}) => ", params.join(", ")));
                self.write_expr(body);
            }
            OirExpr::Map {
                source,
                variable,
                body,
            } => {
                self.write("map(");
                self.write_expr(source);
                self.write(&format!(", {} => ", variable));
                self.write_expr(body);
                self.write(")");
            }
            OirExpr::Filter {
                source,
                variable,
                predicate,
            } => {
                self.write("filter(");
                self.write_expr(source);
                self.write(&format!(", {} => ", variable));
                self.write_expr(predicate);
                self.write(")");
            }
            OirExpr::Reduce {
                source,
                variable,
                accumulator,
                initial,
                body,
            } => {
                self.write("reduce(");
                self.write_expr(source);
                self.write(&format!(", {}, {} => ", variable, accumulator));
                self.write_expr(body);
                self.write(", initial: ");
                self.write_expr(initial);
                self.write(")");
            }
            OirExpr::Effect { effect } => {
                self.write_effect(effect);
            }
        }
    }

    fn write_binding(&mut self, binding: &OirBinding) {
        // `@trait.<Name>[.<slot>]` pretty-prints with the trait name as the
        // second segment, followed by an optional slot. Other roots use their
        // bare name.
        if let OirBindingRoot::Trait(trait_name) = &binding.root {
            if binding.path.is_empty() {
                self.write(&format!("@trait.{}", trait_name));
            } else {
                self.write(&format!("@trait.{}.{}", trait_name, binding.path.join(".")));
            }
            return;
        }

        let root = match &binding.root {
            OirBindingRoot::Entity => "@entity",
            OirBindingRoot::Payload => "@payload",
            OirBindingRoot::State => "@state",
            OirBindingRoot::Config => "@config",
            OirBindingRoot::User => "@user",
            OirBindingRoot::Pages => "@pages",
            OirBindingRoot::CurrentTheme => "@currentTheme",
            OirBindingRoot::Locale => "@locale",
            OirBindingRoot::Event => "@event",
            OirBindingRoot::PrevEvents => "@prevEvents",
            OirBindingRoot::PrevStates => "@prevStates",
            OirBindingRoot::FromState => "@fromState",
            OirBindingRoot::ToState => "@toState",
            OirBindingRoot::Trait(_) => unreachable!("handled above"),
            OirBindingRoot::Local(name) => name,
        };

        if binding.path.is_empty() {
            self.write(root);
        } else {
            self.write(&format!("{}.{}", root, binding.path.join(".")));
        }
    }

    fn format_binary_op(&self, op: &OirBinaryOp) -> &'static str {
        op.symbol()
    }

    fn format_unary_op(&self, op: &OirUnaryOp) -> String {
        op.symbol().to_string()
    }

    fn write_effect(&mut self, effect: &OirEffect) {
        match effect {
            OirEffect::Set { target, value, .. } => {
                self.write("set(");
                self.write_binding(target);
                self.write(", ");
                self.write_expr(value);
                self.write(")");
            }
            OirEffect::SetDynamic { target, value, .. } => {
                self.write("set_dynamic(");
                self.write_expr(target);
                self.write(", ");
                self.write_expr(value);
                self.write(")");
            }
            OirEffect::Emit { event, payload } => {
                self.write(&format!("emit(\"{}\"", event));
                if let Some(p) = payload {
                    self.write(", ");
                    self.write_expr(p);
                }
                self.write(")");
            }
            OirEffect::Fetch {
                entity_type,
                id,
                filter,
                nearest,
                ..
            } => {
                self.write(&format!("fetch(\"{}\"", entity_type));
                if let Some(n) = nearest {
                    self.write(", nearest: ");
                    self.write_expr(n);
                }
                if let Some(id_expr) = id {
                    self.write(", id: ");
                    self.write_expr(id_expr);
                }
                if let Some(f) = filter {
                    self.write(", filter: ");
                    self.write_expr(f);
                }
                self.write(")");
            }
            OirEffect::Persist {
                operation,
                entity_type,
                data,
                ..
            } => {
                self.write(&format!("persist({:?}, \"{}\", ", operation, entity_type));
                self.write_expr(data);
                self.write(")");
            }
            OirEffect::PersistBatch { ops, .. } => {
                self.write("persist-batch(");
                self.write_expr(ops);
                self.write(")");
            }
            OirEffect::Ref {
                entity_type,
                id,
                filter,
                ..
            } => {
                self.write(&format!("ref(\"{}\"", entity_type));
                if let Some(id_expr) = id {
                    self.write(", id: ");
                    self.write_expr(id_expr);
                }
                if let Some(f) = filter {
                    self.write(", filter: ");
                    self.write_expr(f);
                }
                self.write(")");
            }
            OirEffect::Deref { entity_type, id } => {
                self.write(&format!("deref(\"{}\"", entity_type));
                if let Some(id_expr) = id {
                    self.write(", ");
                    self.write_expr(id_expr);
                }
                self.write(")");
            }
            OirEffect::Swap {
                entity_type,
                id,
                transform,
            } => {
                self.write(&format!("swap(\"{}\"", entity_type));
                self.write(", ");
                self.write_expr(id);
                self.write(", ");
                self.write_expr(transform);
                self.write(")");
            }
            OirEffect::Watch {
                entity_type,
                effects,
            } => {
                self.write(&format!("watch(\"{}\", [", entity_type));
                for (i, eff) in effects.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write_expr(eff);
                }
                self.write("])");
            }
            OirEffect::Atomic { effects } => {
                self.write("atomic([");
                for (i, eff) in effects.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write_expr(eff);
                }
                self.write("])");
            }
            OirEffect::Navigate { path, .. } => {
                self.write("navigate(");
                self.write_expr(path);
                self.write(")");
            }
            OirEffect::NavigateBack => {
                self.write("navigate-back()");
            }
            OirEffect::RenderUi { slot, pattern } => {
                self.write(&format!(
                    "render-ui(\"{}\", \"{}\")",
                    slot, pattern.pattern_type
                ));
            }
            OirEffect::CallService {
                service,
                method,
                args,
                ..
            } => {
                self.write(&format!("call-service(\"{}.{}\", ", service, method));
                self.write_expr(args);
                self.write(")");
            }
            OirEffect::Http {
                method,
                origin,
                path,
                request,
                ..
            } => {
                self.write(&format!("http({}, ", format!("{method:?}").to_uppercase()));
                self.write_expr(origin);
                self.write(", ");
                self.write_expr(path);
                if let Some(request) = request {
                    self.write(", ");
                    self.write_expr(request);
                }
                self.write(")");
            }
            OirEffect::Redis {
                origin,
                command,
                options,
                ..
            } => {
                self.write("redis(");
                self.write_expr(origin);
                self.write(", ");
                self.write_expr(command);
                if let Some(options) = options {
                    self.write(", ");
                    self.write_expr(options);
                }
                self.write(")");
            }
            OirEffect::Postgres {
                origin,
                sql,
                params,
                options,
                ..
            } => {
                self.write("postgres(");
                self.write_expr(origin);
                self.write(", ");
                self.write_expr(sql);
                self.write(", ");
                self.write_expr(params);
                if let Some(options) = options {
                    self.write(", ");
                    self.write_expr(options);
                }
                self.write(")");
            }
            OirEffect::CancelCall { key } => {
                self.write("cancel-call(");
                self.write_expr(key);
                self.write(")");
            }
            OirEffect::Os { op, args, .. } => {
                self.write(op.name());
                self.write("(");
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write_expr(arg);
                }
                self.write(")");
            }
            OirEffect::RandomBytes { bytes: expr, .. } => {
                self.write("random-bytes(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::CredentialStatus {
                credential: expr, ..
            } => {
                self.write("credential-status(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::CredentialForget {
                credential: expr, ..
            } => {
                self.write("credential-forget(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::CredentialAdmin { op, request, .. } => {
                self.write(op.name());
                self.write("(");
                self.write_expr(request);
                self.write(")");
            }
            OirEffect::AccessCheck { request, .. } => {
                self.write("access-check(");
                self.write_expr(request);
                self.write(")");
            }
            OirEffect::OAuthAuthorize {
                client, options, ..
            } => {
                self.write("oauth-authorize(");
                self.write_expr(client);
                self.write(", ");
                self.write_expr(options);
                self.write(")");
            }
            OirEffect::OAuthConnectionStatus { provider: expr, .. } => {
                self.write("oauth-connection-status(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::OAuthConnectionForget { provider: expr, .. } => {
                self.write("oauth-connection-forget(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::Spawn { entity_type, props } => {
                self.write(&format!("spawn(\"{}\", ", entity_type));
                self.write_expr(props);
                self.write(")");
            }
            OirEffect::Despawn { entity_id } => {
                self.write("despawn(");
                self.write_expr(entity_id);
                self.write(")");
            }
            OirEffect::Log { level, message } => {
                self.write(&format!("log({:?}, ", level));
                self.write_expr(message);
                self.write(")");
            }
            OirEffect::Custom { name, args, .. } => {
                self.write(&format!("custom(\"{}\"", name));
                for (k, v) in args {
                    self.write(&format!(", {}: ", k));
                    self.write_expr(v);
                }
                self.write(")");
            }
            OirEffect::OsWatchFiles { glob, options, .. } => {
                self.write("os/watch-files(");
                self.write_expr(glob);
                if let Some(o) = options {
                    self.write(", ");
                    self.write_expr(o);
                }
                self.write(")");
            }
            OirEffect::OsWatchProcess {
                name, subcommand, ..
            } => {
                self.write("os/watch-process(");
                self.write_expr(name);
                if let Some(s) = subcommand {
                    self.write(", ");
                    self.write_expr(s);
                }
                self.write(")");
            }
            OirEffect::OsWatchPort { port, protocol, .. } => {
                self.write("os/watch-port(");
                self.write_expr(port);
                if let Some(p) = protocol {
                    self.write(", ");
                    self.write_expr(p);
                }
                self.write(")");
            }
            OirEffect::OsWatchHttp {
                url_pattern,
                method,
                ..
            } => {
                self.write("os/watch-http(");
                self.write_expr(url_pattern);
                if let Some(m) = method {
                    self.write(", ");
                    self.write_expr(m);
                }
                self.write(")");
            }
            OirEffect::OsWatchCron { expression, .. } => {
                self.write("os/watch-cron(");
                self.write_expr(expression);
                self.write(")");
            }
            OirEffect::OsWatchSignal { signal, .. } => {
                self.write("os/watch-signal(");
                self.write_expr(signal);
                self.write(")");
            }
            OirEffect::OsWatchEnv { variable, .. } => {
                self.write("os/watch-env(");
                self.write_expr(variable);
                self.write(")");
            }
            OirEffect::FetchStream {
                entity_type,
                id,
                filter,
                ..
            } => {
                self.write(&format!("fetch-stream({}", entity_type));
                if let Some(id_expr) = id {
                    self.write(", id=");
                    self.write_expr(id_expr);
                }
                if let Some(filter_expr) = filter {
                    self.write(", filter=");
                    self.write_expr(filter_expr);
                }
                self.write(")");
            }
            OirEffect::SendServer { event, payload } => {
                self.write("send-server(event=");
                self.write_expr(event);
                if let Some(p) = payload {
                    self.write(", payload=");
                    self.write_expr(p);
                }
                self.write(")");
            }
            OirEffect::BrowserOpenFilePicker { options, .. } => {
                self.write("browser/open-file-picker(");
                if let Some(o) = options {
                    self.write_expr(o);
                }
                self.write(")");
            }
            OirEffect::BrowserClipboardRead { .. } => {
                self.write("browser/clipboard-read()");
            }
            OirEffect::BrowserClipboardWrite { text, .. } => {
                self.write("browser/clipboard-write(");
                self.write_expr(text);
                self.write(")");
            }
            OirEffect::BrowserGeolocationCurrent { options, .. } => {
                self.write("browser/geolocation-current(");
                if let Some(o) = options {
                    self.write_expr(o);
                }
                self.write(")");
            }
            OirEffect::BrowserPushSubscribe { .. } => {
                self.write("browser/push-subscribe()");
            }
            OirEffect::BrowserPageFacts { fields, .. } => {
                let words: Vec<&str> = fields.iter().map(|f| f.as_str()).collect();
                self.write(&format!("browser/page-facts([{}])", words.join(", ")));
            }
            OirEffect::BrowserPrompt {
                text, schema, key, ..
            } => {
                self.write("browser/prompt(");
                self.write_expr(text);
                self.write(", ");
                self.write_expr(schema);
                if let Some(k) = key {
                    self.write(", ");
                    self.write_expr(k);
                }
                self.write(")");
            }
            OirEffect::BrowserEmbed { texts, key, .. } => {
                self.write("browser/embed(");
                self.write_expr(texts);
                if let Some(k) = key {
                    self.write(", ");
                    self.write_expr(k);
                }
                self.write(")");
            }
            OirEffect::BrowserTab { op, request, key, .. } => {
                self.write(op.name());
                self.write("(");
                self.write_expr(request);
                if let Some(k) = key {
                    self.write(", ");
                    self.write_expr(k);
                }
                self.write(")");
            }
            OirEffect::BrowserBeacon {
                url, request, key, ..
            } => {
                self.write("browser/beacon(");
                self.write_expr(url);
                self.write(", ");
                self.write_expr(request);
                if let Some(k) = key {
                    self.write(", ");
                    self.write_expr(k);
                }
                self.write(")");
            }
        }
    }
}

impl Default for PrettyPrinter {
    fn default() -> Self {
        Self::new()
    }
}

/// Print a module to a string
pub fn pretty_print(module: &OirModule) -> String {
    PrettyPrinter::new().print_module(module)
}

/// Print a module with custom config
pub fn pretty_print_with_config(module: &OirModule, config: PrettyConfig) -> String {
    PrettyPrinter::with_config(config).print_module(module)
}

/// Print an expression to a string
pub fn pretty_print_expr(expr: &OirExpr) -> String {
    PrettyPrinter::new().print_expr(expr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{OirEvent, OirModule};

    #[test]
    fn row_prints_as_the_bare_entity_where_reference_prints_an_arrow() {
        let p = PrettyPrinter::new();
        assert_eq!(
            p.format_type(&OirType::array(OirType::row("Revision"))),
            "[Revision]"
        );
        assert_eq!(
            p.format_type(&OirType::array(OirType::reference("Revision"))),
            "[-> Revision]"
        );
    }

    #[test]
    fn test_pretty_print_module() {
        let mut module = OirModule::new("TestApp", "1.0.0");
        module.description = Some("A test application".to_string());

        let mut entity = OirEntity::new("Task");
        entity.fields.push(OirField::new("title", OirType::String));
        entity
            .fields
            .push(OirField::optional("description", OirType::String));
        module.entities.push(entity);

        let mut trait_def = OirTrait::new("TaskLifecycle");
        trait_def.states.push(OirState::initial("idle"));
        trait_def.states.push(OirState::new("running"));
        trait_def.events.push(OirEvent::new("START"));
        trait_def
            .transitions
            .push(OirTransition::new("idle", "running", "START"));
        module.traits.push(trait_def);

        let output = pretty_print(&module);
        assert!(output.contains("Module: TestApp v1.0.0"));
        assert!(output.contains("entity Task"));
        assert!(output.contains("trait TaskLifecycle"));
    }

    #[test]
    fn test_pretty_print_expr() {
        let expr = OirExpr::and(
            OirExpr::eq(
                OirExpr::entity_binding(vec!["status".to_string()]),
                OirExpr::string("active"),
            ),
            OirExpr::gt(
                OirExpr::entity_binding(vec!["count".to_string()]),
                OirExpr::number(0.0),
            ),
        );

        let output = pretty_print_expr(&expr);
        assert!(output.contains("@entity.status"));
        assert!(output.contains("=="));
        assert!(output.contains("\"active\""));
    }
}
