//! Pages, their sections, patterns and bindings.

use super::*;

/// Page definition (UI)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirPage {
    pub name: String,
    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub path: String,
    /// Declared nav label (`@label`); absent = the page name as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `title:` page modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<crate::schema::types::PageMeta>,
    /// `description:` page modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<crate::schema::types::PageMeta>,
    /// `access:` page modifier; absent = no gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<crate::schema::types::PageAccess>,
    /// `indexing:` page modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<crate::schema::types::PageIndexing>,
    /// The upstream page this one was imported from; pages sharing it in
    /// different locales are language alternates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_page: Option<String>,
    /// `translationOf:` page modifier; groups alternates ahead of `source_page`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_type: Option<OirViewType>,
    #[serde(default)]
    pub sections: Vec<OirSection>,
    #[serde(default)]
    pub traits: Vec<OirTraitBinding>,
    #[serde(default)]
    pub entity_bindings: Vec<OirEntityBinding>,
    #[serde(default)]
    pub navigation: Vec<OirNavigation>,
}

impl OirPage {
    /// Create a new page
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: None,
            label: None,
            path: path.into(),
            title: None,
            description: None,
            access: None,
            indexing: None,
            source_page: None,
            translation_of: None,
            layout: None,
            view_type: None,
            sections: vec![],
            traits: vec![],
            entity_bindings: vec![],
            navigation: vec![],
        }
    }

    /// Find a section by id
    pub fn find_section(&self, id: &str) -> Option<&OirSection> {
        self.sections.iter().find(|s| s.id == id)
    }
}

/// View type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OirViewType {
    Dashboard,
    List,
    Detail,
    Create,
    Edit,
    Custom(String),
}

/// Section definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirSection {
    pub id: String,
    pub pattern: OirPattern,
    #[serde(default)]
    pub events: Vec<OirSectionEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
}

impl OirSection {
    /// Create a new section
    pub fn new(id: impl Into<String>, pattern: OirPattern) -> Self {
        Self {
            id: id.into(),
            pattern,
            events: vec![],
            position: None,
        }
    }
}

/// Pattern definition
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirPattern {
    /// Pattern type: "entity-table", "form-section", etc.
    pub pattern_type: String,
    /// Pattern configuration (varies by pattern)
    #[serde(default)]
    pub config: OirObject,
}

impl OirPattern {
    /// Create a new pattern
    pub fn new(pattern_type: impl Into<String>) -> Self {
        Self {
            pattern_type: pattern_type.into(),
            config: OirObject::default(),
        }
    }

    /// Add configuration
    pub fn with_config(mut self, key: impl Into<String>, value: OirValue) -> Self {
        self.config.insert(key.into(), value);
        self
    }
}

/// Section event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirSectionEvent {
    /// Trigger: "click", "submit", etc.
    pub trigger: String,
    /// Effect to execute
    pub action: crate::ir::expr::OirExpr,
}

/// Trait binding
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirTraitBinding {
    pub trait_name: String,
    /// V4 dual-carry id sibling of `trait_name` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trait_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<OirObject>,
}

impl OirTraitBinding {
    /// Create a new trait binding
    pub fn new(trait_name: impl Into<String>) -> Self {
        Self {
            trait_name: trait_name.into(),
            trait_id: None,
            config: None,
        }
    }
}

/// Entity binding
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirEntityBinding {
    pub entity_name: String,
    /// V4 dual-carry id sibling of `entity_name` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<crate::ir::expr::OirExpr>,
}

impl OirEntityBinding {
    /// Create a new entity binding
    pub fn new(entity_name: impl Into<String>) -> Self {
        Self {
            entity_name: entity_name.into(),
            entity_id: None,
            alias: None,
            filter: None,
        }
    }
}

/// Navigation item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirNavigation {
    pub label: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}
