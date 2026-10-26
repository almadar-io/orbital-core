//! Building a `BindingContext` field by field.

use super::*;

/// Builder for creating binding contexts
#[derive(Debug, Default)]
pub struct BindingContextBuilder {
    pub(super) context: BindingContext,
}

impl BindingContextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn now(mut self, now: f64) -> Self {
        self.context.now = Some(now);
        self
    }

    pub fn entity(mut self, entity: Value) -> Self {
        self.context.entity = SharedEntity::whole(entity);
        self
    }

    pub fn payload(mut self, payload: Value) -> Self {
        self.context.payload = Arc::new(payload);
        self
    }

    pub fn state(mut self, state: Value) -> Self {
        self.context.state = Arc::new(state);
        self
    }

    pub fn config(mut self, config: Value) -> Self {
        self.context.config = Arc::new(config);
        self
    }

    pub fn user(mut self, user: Value) -> Self {
        self.context.user = Arc::new(user);
        self
    }

    pub fn current_theme(mut self, current_theme: Value) -> Self {
        self.context.current_theme = Arc::new(current_theme);
        self
    }

    pub fn pages(mut self, pages: Value) -> Self {
        self.context.pages = Arc::new(pages);
        self
    }

    pub fn locale(mut self, locale: Value) -> Self {
        self.context.locale = Arc::new(locale);
        self
    }

    pub fn messages(mut self, messages: HashMap<String, String>) -> Self {
        self.context.messages = Arc::new(messages);
        self
    }

    pub fn local(mut self, name: &str, value: Value) -> Self {
        self.context.set_local(name, value);
        self
    }

    pub fn entity_store(mut self, entity_type: &str, entities: Vec<Value>) -> Self {
        Arc::make_mut(&mut self.context.entity_stores).insert(entity_type.to_string(), entities);
        self
    }

    pub fn build(self) -> BindingContext {
        self.context
    }
}
