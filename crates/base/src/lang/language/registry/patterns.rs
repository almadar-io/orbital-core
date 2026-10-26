//! Embedded UI patterns, their props and payload shapes, installed behaviors and theme names.

use super::*;

/// The raw patterns-registry.json text, from the installed `@almadar/core`.
pub fn patterns_json() -> &'static str {
    crate::language_data::language_data_after_preflight()
        .patterns
        .as_str()
}

// ============================================================================
// 3.1b — Built-in theme names, from the installed @almadar/core presets
// ============================================================================

/// The `[data-theme]` keys of every installed `@almadar/core` preset
/// (`<name>-light` / `<name>-dark`, the selectors `@almadar/ui`'s generated
/// theme CSS defines). These (plus any orbital-declared inline theme names)
/// form the valid-theme set the theme-reference validator accepts.
pub fn builtin_theme_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| crate::theme::ThemePresets::installed().theme_keys())
}

// ============================================================================
// 3.1c — Embedded services-registry.json (call-service integrator contracts)
// ============================================================================


/// A single field of an emit/listen payload schema, mirroring trait
/// `payloadSchema` entries so L1/L2 consumers can compare the component's
/// documented bus shape against the trait's declared `emits {}` payload.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedPayloadField {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    /// Declared knob-conditional type list (component JSDoc's
    /// `@payloadTypeWhen`, pattern-synced into the registry as `typeWhen`).
    /// The render-ui binding check (`phases/validation/render_ui/binding.rs`)
    /// evaluates this against the consuming trait's own resolved config —
    /// the SAME `resolve_payload_type_when` the inline narrowing pass uses.
    #[serde(rename = "typeWhen", default)]
    pub type_when: Option<Vec<PayloadTypeWhen>>,
    /// `event` when the member's TS type is `EventKey`.
    #[serde(default)]
    pub kind: Option<String>,
    /// `@payloadFor <sibling>`: the event-kind sibling this member carries the payload of.
    #[serde(rename = "payloadFor", default)]
    pub payload_for: Option<String>,
}

/// A single positional arg of a React callback prop. Used by L2 to verify
/// name-and-type parity between the callback and the trait's declared event
/// payload (gap #32).
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedCallbackArg {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    /// Nested structural shape for an object-typed arg (e.g. a canvas
    /// `onShapeClick` payload `{ id?, type?, index }`). The pattern-sync
    /// parser has always written this into `patterns-registry.json`; this
    /// struct used to drop it, forcing listen contracts down to a shapeless
    /// `payload : object`. Reuses `EmbeddedPatternProp` — the same
    /// `types`/`properties`/`required` nesting the JSON carries.
    #[serde(default)]
    pub schema: Option<Box<EmbeddedPatternProp>>,
}

/// JSON-schema-style `required` field — accepts either a boolean (used at
/// the pattern-prop top level, e.g. `columns: { required: true }`) OR an
/// array of property names (used at the nested-object level, e.g.
/// `items: { properties: { id: { types: ["string"] } }, required: ["id"] }`).
/// Pattern-sync writes both shapes; consumers use helpers below to read.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RequiredFlag {
    Bool(bool),
    Names(Vec<String>),
}

impl Default for RequiredFlag {
    fn default() -> Self {
        RequiredFlag::Bool(false)
    }
}

impl RequiredFlag {
    /// Whether the property carrying this flag is required ON ITS PARENT object —
    /// the `Bool(true)` form ONLY. The `Names([...])` form means something
    /// different ("these sub-keys are required WITHIN this property's own object,
    /// if the property is present"), read via [`as_names`](Self::as_names); it must
    /// NOT make the property itself parent-required. (The old `as_bool` conflated
    /// the two — `Names` non-empty → true — which wrongly demanded e.g. an optional
    /// `relation` on every `form-section` field.)
    pub fn required_on_parent(&self) -> bool {
        matches!(self, RequiredFlag::Bool(true))
    }

    /// Read as a slice of field names — the `Names([...])` form's required
    /// sub-keys; empty for the bool form.
    pub fn as_names(&self) -> &[String] {
        match self {
            RequiredFlag::Bool(_) => &[],
            RequiredFlag::Names(v) => v.as_slice(),
        }
    }
}

/// A single prop entry in a pattern's `propsSchema`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedPatternProp {
    #[serde(default)]
    pub types: Vec<String>,
    /// Bounded string-literal union members (e.g. `VisualStyle`, button
    /// `variant`), present at any nesting level — top prop OR a nested
    /// `properties`/`items` field. Codegen consults this to drop an empty
    /// enum sub-field instead of emitting an invalid `""` (CG-SWEEP-1 Cat 11).
    #[serde(rename = "enumValues", default)]
    pub enum_values: Option<Vec<String>>,
    #[serde(default)]
    pub required: RequiredFlag,
    #[serde(default)]
    pub description: Option<String>,
    /// Optional marker for SCALAR props whose value is a field name on the
    /// linked entity. Currently the only recognised value is `"field-ref"`.
    /// See `docs/Almadar_Agent_Behaviors_Plan.md` Phase 3.2/3.3.
    ///
    /// Pattern-sync also writes the F1 plan's event-related kinds here:
    /// `"event"` (legacy, EventKey alias), `"event-ref"` (declarative
    /// bus-emit prop, sourced from `EventEmit<P>`), `"event-listen"`
    /// (`EventListen<P>` mirror), `"event-list"` (array prop whose elements
    /// carry an event-name field), `"event-map"` (dynamic-key object prop
    /// whose VALUES — not the prop itself — are declared event keys, e.g. a
    /// d-pad's direction→event map or a canvas's keycode→event map; the
    /// Record/index-signature value type resolved to `EventKey`),
    /// `"callback"` (React callback prop bound to an event name at the
    /// schema-author site — NEVER set for a render-prop that returns a
    /// value, see `renderCallback` below).
    #[serde(default)]
    pub kind: Option<String>,
    /// Optional marker for ARRAY props whose string elements are field names
    /// on the linked entity. Currently the only recognised value is
    /// `"field-ref"`. Non-string array elements are ignored by the resolver.
    #[serde(rename = "itemKind", default)]
    pub item_kind: Option<String>,
    /// For `kind: "event-list"`: which field inside each array item holds
    /// the event key. Default is `"event"` — set explicitly when the
    /// descriptor type uses a different field name.
    #[serde(rename = "eventField", default)]
    pub event_field: Option<String>,
    /// `@notification`: the component reports a change it already applied (hover, close
    /// finished, resize), so a final program may leave the event unhandled (G-ORB-187).
    #[serde(default)]
    pub notification: bool,
    /// For `kind: "event-ref"` whose source is `EventEmit<P>`: the
    /// structural shape of `P`, as the bus payload the component fires
    /// when the prop is bound. Validator (L2.2) compares this against the
    /// trait's declared `emits { EVENT { ... } }` payload.
    #[serde(rename = "emitPayloadSchema", default)]
    pub emit_payload_schema: Option<Vec<EmbeddedPayloadField>>,
    /// For `kind: "event-listen"` whose source is `EventListen<P>`: the
    /// structural shape of `P`, as the payload the component subscribes to.
    #[serde(rename = "listenPayloadSchema", default)]
    pub listen_payload_schema: Option<Vec<EmbeddedPayloadField>>,
    /// For `kind: "callback"`: positional parameter list of the function
    /// type. Validator uses the names to build the param-name → payload-
    /// field map for the C2 codegen + runtime callback wrap.
    #[serde(rename = "callbackArgs", default)]
    pub callback_args: Option<Vec<EmbeddedCallbackArg>>,
    /// For array-typed props: per-element shape. Carried for the
    /// `ORB_RUI_ELEMENT_SHAPE_MISMATCH` rule so the validator can check
    /// each element of a bound array literal against the component's
    /// required element fields (e.g. `MarketingFooter.columns` →
    /// `items.properties = { title: required, items: required }`).
    #[serde(default)]
    pub items: Option<Box<EmbeddedPatternProp>>,
    /// For object-typed props OR for elements of `items`: per-field shape.
    /// Required fields here are enforced by the element-shape rule against
    /// the lolo literal's keys.
    #[serde(default)]
    pub properties: Option<HashMap<String, EmbeddedPatternProp>>,
    /// Entity-row clean slate: when a prop's TS type is `EntityWith<K>`,
    /// pattern-sync writes the keys `K` here. The bound entity (or a remap
    /// field-map) MUST provide every one of these fields, else the component
    /// renders blank rows. Enforced by `ORB_X_ENTITY_PROP_CONTRACT`.
    #[serde(rename = "requiredFields", default)]
    pub required_fields: Option<Vec<String>>,
    /// `@payloadFor <sibling>` pairing read off THIS struct member (not an
    /// enclosing prop — a struct member declared inside an array-item
    /// interface, e.g. `FloatingToolbarItem.actionPayload`, has no parent
    /// `ExtractedProp` to surface it on). Names a sibling key in this SAME
    /// `properties` map, itself carrying `kind: "event"`. The render-ui
    /// binding check (`check_callback_arg_schema`) uses this pair to derive
    /// the correct expected type (`event` / `@payload(.sibling)`) instead of
    /// falling back to `types.first()` ("string"/"object"), which produced a
    /// false `ORB_RUI_EVENT_BINDING_PAYLOAD_MISMATCH` against the trait's
    /// honestly-declared dependent type.
    #[serde(rename = "payloadFor", default)]
    pub payload_for: Option<String>,
}

/// A single pattern entry from `patterns-registry.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedPattern {
    #[serde(rename = "type", default)]
    pub pattern_type: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "propsSchema", default)]
    pub props_schema: HashMap<String, EmbeddedPatternProp>,
    #[serde(rename = "entityAware", default)]
    pub entity_aware: bool,
    /// `form` for a pattern that collects field values and submits them (`form`, `form-section`).
    #[serde(rename = "fieldsContract", default)]
    pub fields_contract: Option<String>,
}

pub static PATTERNS: OnceLock<HashMap<String, EmbeddedPattern>> = OnceLock::new();

/// All patterns keyed by pattern type (e.g. `"table-view"`, `"button"`).
pub fn patterns() -> &'static HashMap<String, EmbeddedPattern> {
    PATTERNS.get_or_init(|| {
        // patterns-registry.json shape: { version, exportedAt, patterns: { name: PatternEntry } }
        let parsed: serde_json::Value = serde_json::from_str(patterns_json())
            .expect("installed patterns-registry.json is corrupt — reinstall @almadar/core");
        let patterns_obj = parsed
            .get("patterns")
            .and_then(|v| v.as_object())
            .expect("patterns-registry.json missing 'patterns' object at root");
        let mut map = HashMap::new();
        for (name, value) in patterns_obj {
            if let Ok(p) = serde_json::from_value::<EmbeddedPattern>(value.clone()) {
                map.insert(name.clone(), p);
            }
        }
        map
    })
}

/// Returns true if `pattern_type` is a known pattern.
pub fn is_known_pattern(pattern_type: &str) -> bool {
    patterns().contains_key(pattern_type)
}

/// Look up a pattern entry by type.
pub fn get_pattern(pattern_type: &str) -> Option<&'static EmbeddedPattern> {
    patterns().get(pattern_type)
}

// ============================================================================
// 3.3 — Known-behavior index (every behavior package is installed, not baked)
// ============================================================================
//
// `@almadar/std` and `@almadar-io/behaviors` (and any other behavior package)
// are installed, not baked: a name is known when an installed package ships a
// behavior file of that name (`behavior_index::indexed_names`).

#[cfg(feature = "std")]
/// Returns true if `name` is a behavior an installed behavior package ships. This
/// call site has no `LoaderConfig`/`packages` to reuse, so it re-discovers from
/// `current_dir()` / the user store, same as `CompilerOptions::from_env`.
pub fn is_known_behavior(name: &str) -> bool {
    installed_behavior_index_contains(name)
}

#[cfg(feature = "std")]
/// True when `name` is shipped by any installed behavior package. Package discovery +
/// each package's names are memoized per registry root.
pub fn installed_behavior_index_contains(name: &str) -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let user_store = crate::behavior_packages::user_store_dir().unwrap_or_else(|| cwd.clone());
    let Ok(packages) = crate::behavior_packages::discover(&cwd, &user_store) else {
        return false;
    };
    packages
        .iter()
        .any(|pkg| package_behavior_names(pkg).contains(name))
}

#[cfg(feature = "std")]
pub fn package_behavior_names(pkg: &crate::behavior_packages::BehaviorPackage) -> HashSet<String> {
    use std::sync::Mutex;
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, HashSet<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = pkg.registry.root().to_path_buf();
    if let Some(names) = cache
        .lock()
        .expect("package behavior name cache poisoned")
        .get(&key)
    {
        return names.clone();
    }
    let names = crate::behavior_index::indexed_names(pkg).unwrap_or_default();
    cache
        .lock()
        .expect("package behavior name cache poisoned")
        .insert(key, names.clone());
    names
}
