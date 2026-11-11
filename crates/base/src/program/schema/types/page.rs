//! Page definitions, page metadata and the domain and design hints.

use super::*;

// =============================================================================
// Page Definition
// =============================================================================

/// Page (route) definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageDefinition {
    /// Page name
    pub name: String,

    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Route path. A protocol page has an empty path; its `address` carries where it is reached.
    pub path: String,

    /// Endpoint protocol (`http`, `stdio`, `boot`); absent = a route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,

    /// The protocol's address record (`page http { host: …, port: … }`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<SExpression>,

    /// View type
    #[serde(rename = "viewType", skip_serializing_if = "Option::is_none")]
    pub view_type: Option<String>,

    /// Primary entity for this page
    #[serde(rename = "primaryEntity", skip_serializing_if = "Option::is_none")]
    pub primary_entity: Option<String>,

    /// Whether this is the initial page
    #[serde(rename = "isInitial", skip_serializing_if = "Option::is_none")]
    pub is_initial: Option<bool>,

    /// Whether this is the page shown for a path no page matches (`[notFound]`)
    #[serde(rename = "isNotFound", default, skip_serializing_if = "Option::is_none")]
    pub is_not_found: Option<bool>,

    /// Traits active on this page
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<TraitReference>>,

    /// Page sections
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<SectionDefinition>>,

    /// Nav icon name — `@icon "..."` annotation on the page declaration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Nav label — `@label "..."` annotation. Only a page that declares one is
    /// a `@pages` nav entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Roles the nav entry shows for — `@roles "a, b"` annotation. Absent = everyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,

    /// `access:` page modifier. Absent = the app's authored behavior, no gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PageAccess>,

    /// `indexing:` page modifier — a search directive, never access control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<PageIndexing>,

    /// `title:` page modifier — the document title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<PageMeta>,

    /// `description:` page modifier — the document meta description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<PageMeta>,
    /// The upstream page this one was imported from (its id, kept through
    /// nested imports). Imports of one upstream page in different locales are
    /// language alternates of each other. Set by inlining, never authored.
    #[serde(
        rename = "sourcePage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_page: Option<String>,
    /// `translationOf:` page modifier — pages sharing it in different locales
    /// are language alternates (takes precedence over `sourcePage`).
    #[serde(
        rename = "translationOf",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub translation_of: Option<String>,
}

/// One `@pages` nav entry: a root route page that declares its nav `@label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavEntry {
    pub href: String,
    pub label: String,
    pub icon: Option<String>,
    pub roles: Option<Vec<String>>,
}

/// `@pages` of the pages given, in their order: each inline route page that declares a nav `@label` and has no
/// `:param` in its path, once per path (the first wins). A page reference is no entry: it is resolved before here.
pub fn nav_entries<'a>(pages: impl IntoIterator<Item = &'a PageRef>) -> Vec<NavEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    for page in pages.into_iter().filter_map(PageRef::as_inline).filter(|page| page.is_route()) {
        let Some(label) = &page.label else { continue };
        if page.path.contains(':') || !seen.insert(page.path.clone()) {
            continue;
        }
        entries.push(NavEntry { href: page.path.clone(), label: label.clone(), icon: page.icon.clone(), roles: page.roles.clone() });
    }
    entries
}

#[cfg(test)]
mod nav_entry_tests {
    use super::*;

    fn pages(json: serde_json::Value) -> Vec<PageRef> {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn g_orb_282_nav_entries_are_the_labelled_root_route_pages_once_per_path_in_order() {
        let listed = pages(serde_json::json!([
            { "name": "Home", "path": "/", "label": "Home", "icon": "home" },
            { "name": "Team", "path": "/team", "label": "Team", "roles": ["admin"] },
            { "name": "TeamAgain", "path": "/team", "label": "Team twice" },
            { "name": "Detail", "path": "/team/:id", "label": "Member" },
            { "name": "Hidden", "path": "/hidden" },
            "Other.pages.Linked"
        ]));
        let entries = nav_entries(&listed);
        assert_eq!(
            entries,
            vec![
                NavEntry { href: "/".into(), label: "Home".into(), icon: Some("home".into()), roles: None },
                NavEntry { href: "/team".into(), label: "Team".into(), icon: None, roles: Some(vec!["admin".into()]) },
            ],
            "a second page on one path, a :param page, an unlabelled page and a page reference are no entries"
        );
    }

    #[test]
    fn g_orb_282_a_page_of_another_protocol_is_no_nav_entry() {
        let listed = pages(serde_json::json!([
            { "name": "Boot", "path": "/boot", "protocol": "boot", "label": "Boot" },
            { "name": "Home", "path": "/", "label": "Home" }
        ]));
        let hrefs: Vec<String> = nav_entries(&listed).into_iter().map(|entry| entry.href).collect();
        assert_eq!(hrefs, vec!["/".to_string()], "control: the route page beside it is listed");
    }
}

impl PageDefinition {
    pub fn is_route(&self) -> bool {
        self.protocol.is_none()
    }

    pub fn protocol_name(&self) -> &str {
        self.protocol.as_deref().unwrap_or(crate::protocols::ROUTE)
    }
}

/// Who may open a page: `public` admits anonymous viewers; `authenticated`
/// mounts the page circuit only for a resolved signed-in viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageAccess {
    Public,
    Authenticated,
}

/// Search-engine directive for a page's document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageIndexing {
    Index,
    Noindex,
}

/// A page metadata value: a literal, or `(i18n/t "key")` resolved against the
/// route's locale. Nothing else may bind (no entity/config/viewer frame).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PageMeta {
    Literal(String),
    Message(PageMessage),
}

/// `["i18n/t", "<key>"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageMessage(pub I18nHead, pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum I18nHead {
    #[serde(rename = "i18n/t")]
    T,
}

impl PageMeta {
    pub fn message(key: impl Into<String>) -> Self {
        PageMeta::Message(PageMessage(I18nHead::T, key.into()))
    }

    /// The literal text, or the message resolved through `lookup`.
    pub fn resolve<'a>(&'a self, lookup: impl Fn(&str) -> Option<&'a str>) -> Option<&'a str> {
        match self {
            PageMeta::Literal(s) => Some(s),
            PageMeta::Message(PageMessage(_, key)) => lookup(key),
        }
    }
}

/// The page modifiers a `pages {}` remap entry may declare for one imported page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PageModifiers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PageAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<PageIndexing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<PageMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<PageMeta>,
    #[serde(
        rename = "translationOf",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub translation_of: Option<String>,
    /// The page's nav entry label (a `pages {}` remap entry's `label:`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The page's nav entry icon (a `pages {}` remap entry's `icon:`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The roles the page's nav entry shows for (a `pages {}` remap entry's `roles:`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
}

/// The sign-in route every host provides itself (the compiled shell's login form, the
/// runtime host's sign-in surface). `signIn:` may name it without a page, and no page
/// may be declared on it.
pub const HOST_SIGN_IN_ROUTE: &str = "/login";

/// Whether `path` is [`HOST_SIGN_IN_ROUTE`], ignoring a trailing slash.
pub fn is_host_sign_in_route(path: &str) -> bool {
    path.trim_end_matches('/') == HOST_SIGN_IN_ROUTE
}

/// App-level site modifiers from the `app … [ … ]` bracket.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SiteMeta {
    /// Canonical production origin, e.g. `https://orb.almadar.io`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Site icon path served from the public folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Site name (og:site_name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Route an `access: authenticated` page sends a non-admitted viewer to.
    #[serde(rename = "signIn", default, skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<String>,
    /// Route paths a viewer reaches from outside the app (a URL the CLI, an email or another app opens), so no
    /// link inside the app has to lead to them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<String>,
}

impl SiteMeta {
    pub fn is_empty(&self) -> bool {
        *self == SiteMeta::default()
    }
}

/// Section definition within a page
/// DEPRECATED: Use render-ui effects instead
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionDefinition {
    /// Section slot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,

    /// Pattern type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<PatternDefinition>,

    /// Props
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props: Option<crate::schema::sexpr::SObject>,
}

/// Pattern definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternDefinition {
    #[serde(rename = "type")]
    pub pattern_type: String,
}

// =============================================================================
// Domain & Design
// =============================================================================

/// Domain context metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vocabulary: Option<crate::collections::IndexMap<String, String>>,
    #[serde(rename = "requestFragment", skip_serializing_if = "Option::is_none")]
    pub request_fragment: Option<String>,
}

/// Design hints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignHints {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(rename = "uxHints", skip_serializing_if = "Option::is_none")]
    pub ux_hints: Option<crate::collections::IndexMap<String, String>>,
}
