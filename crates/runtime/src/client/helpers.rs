//! Pattern Rendering Helpers
//!
//! Common utilities for extracting and processing pattern properties
//! from orbital schema definitions.

use orbital_core_base::value::ValueMap;
use orbital_core_base::value::Value;
use std::collections::HashMap;

// Calendar derivation helpers ride the same glob import every generated renderer
// already has (`use crate::client::helpers::*;`).
pub use super::calendar::*;

/// The named-color table both color parsers share — one source of truth so egui
/// and DOM/SVG backends accept the same names and resolve them to identical RGB.
pub const NAMED_COLORS: &[(&str, (u8, u8, u8))] = &[
    ("red", (220, 60, 60)),
    ("green", (60, 180, 90)),
    ("blue", (70, 120, 220)),
    ("yellow", (230, 200, 60)),
    ("orange", (230, 140, 50)),
    ("purple", (150, 90, 200)),
    ("gray", (128, 128, 128)),
    ("grey", (128, 128, 128)),
];

/// The DOM/SVG twin of `parse_css_color`: same acceptance (hex `#rgb`/`#rrggbb` or
/// the shared name table), returning a normalized `#rrggbb` string. `None` for
/// unrecognized input so callers fall back to a palette slot exactly like egui —
/// this is what keeps placeholder junk (e.g. `"color": "Color"` in std sample
/// data) from rendering as SVG's default black.
pub fn parse_color_hex(s: &str) -> Option<String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        let all_hex = |h: &str| h.chars().all(|c| c.is_ascii_hexdigit());
        return match hex.len() {
            6 if all_hex(hex) => Some(format!("#{}", hex.to_ascii_uppercase())),
            3 if all_hex(hex) => {
                let b = hex.as_bytes();
                Some(format!(
                    "#{0}{0}{1}{1}{2}{2}",
                    (b[0] as char).to_ascii_uppercase(),
                    (b[1] as char).to_ascii_uppercase(),
                    (b[2] as char).to_ascii_uppercase()
                ))
            }
            _ => None,
        };
    }
    let lower = s.to_ascii_lowercase();
    NAMED_COLORS
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, (r, g, b))| format!("#{:02X}{:02X}{:02X}", r, g, b))
}

/// The shared categorical palette as `#rrggbb` strings — the one source of truth
/// for every backend. `palette_color` (egui) and `palette_hex` (DOM/SVG backends)
/// must stay in lockstep; both index this table.
pub const PALETTE_HEX: [&str; 8] = [
    "#4678DC", "#3CB45A", "#E68C32", "#965AC8", "#DC3C3C", "#28AAB4", "#E6C83C", "#C864A0",
];

/// The same categorical color as a `#rrggbb` string for DOM/SVG backends.
pub fn palette_hex(i: usize) -> &'static str {
    PALETTE_HEX[i % PALETTE_HEX.len()]
}

/// Resolve a datum/series/node color as a hex string: an explicit `color` field
/// if it parses (same rule as egui), else a palette slot by index. DOM/SVG twin
/// of `item_color`.
pub fn item_color_hex(item: &Value, index: usize) -> String {
    get_string(item, "color")
        .and_then(|s| parse_color_hex(&s))
        .unwrap_or_else(|| palette_hex(index).to_string())
}

/// Resolve a color *prop* for DOM/SVG backends: the prop value if it parses under
/// the shared rule (`parse_color_hex`), else `default` (callers keep it a valid
/// `#rrggbb` — palette slots qualify). DOM/SVG twin of the egui idiom
/// `parse_css_color(&get_string_or(props, key, default)).unwrap_or(default)` — one
/// parse rule, one fallback contract, no renderer-specific color drift.
pub fn get_color_or(props: &Value, key: &str, default: &str) -> String {
    get_string(props, key)
        .and_then(|s| parse_color_hex(&s))
        .unwrap_or_else(|| default.to_string())
}

/// Backend-erased "render one child pattern" callback: `(ui_ptr, child, ctx)`.
/// `ui_ptr` is the backend's live `ui`/`frame` type-erased to `*mut ()`; the
/// backend's closure casts it back and dispatches the child through its
/// `PatternRegistry`. Keeps `RenderContext` free of any `egui`/`ratatui` type.
pub type ChildRenderFn<'a> = &'a dyn for<'b> Fn(*mut (), &Value, &mut RenderContext<'b>);

/// Mutable runtime state passed to pattern renderers.
pub struct RenderContext<'a> {
    /// Name of the currently active trait.
    pub trait_name: &'a str,
    /// Name of the currently active orbital.
    pub orbital_name: &'a str,
    /// Name of the current state machine state.
    pub current_state: &'a str,
    /// Cached entity data returned by the server.
    pub entities: &'a HashMap<String, Vec<Value>>,
    /// Current form field values.
    pub form_data: &'a mut HashMap<String, String>,
    /// Entity being edited, used for form pre-population.
    pub editing_entity: &'a Option<Value>,
    /// Events queued during rendering to be sent after the frame.
    pub pending_events: &'a mut Vec<(String, Option<String>, Value)>,
    /// Backend recursion hook — lets a layout/container renderer render its
    /// `children`. `None` disables recursion (e.g. leaf-only contexts).
    pub child_render: Option<ChildRenderFn<'a>>,
    /// Per-trait current render output, keyed by trait name. Used to resolve
    /// `@trait.X` child references (matches `@almadar/ui`'s `<TraitFrame>`).
    pub trait_content: &'a ValueMap,
    /// The trait that authored the content currently being rendered — the *source*
    /// of any event a button here queues. Set to a slot's origin trait at the top and
    /// switched per `@trait.X` child as rendering descends, so a button nested under
    /// `@trait.InventoryCatalog` emits with source `InventoryCatalog` (which the modal
    /// trait's `listens` matches). `None` falls back to `trait_name`.
    pub source_trait: Option<String>,
    /// The kernel's `canvasTheme`, handed to every Rust-drawn scene (`None` without a kernel).
    pub canvas_theme: Option<&'a serde_json::Value>,
}

impl<'a> RenderContext<'a> {
    /// Queue a user-initiated event (button/submit). It dispatches with the *source
    /// trait* of the content that rendered this control (the trait whose `listens`/
    /// transitions should route it) — falling back to the active `trait_name`. The
    /// kernel's cross-trait fan-out then reaches the modal/persist traits that listen
    /// for it.
    pub fn queue_event(&mut self, event: impl Into<String>, payload: Value) {
        let event = event.into();
        let source = self
            .source_trait
            .clone()
            .unwrap_or_else(|| self.trait_name.to_string());
        tracing::debug!(
            target: "almadar::client",
            event = %event,
            source = %source,
            active_trait = %self.trait_name,
            "queue_event (component click)"
        );
        self.pending_events.push((event, Some(source), payload));
    }

    /// Get a mutable form entry, creating an empty one if absent.
    pub fn form_entry(&mut self, key: impl Into<String>) -> &mut String {
        self.form_data.entry(key.into()).or_default()
    }

    /// Access the entire form data map.
    pub fn form_data(&self) -> &HashMap<String, String> {
        self.form_data
    }

    /// Access the entire form data map mutably.
    pub fn form_data_mut(&mut self) -> &mut HashMap<String, String> {
        self.form_data
    }

    /// Return cached data for an entity type, if any.
    pub fn entity_data(&self, entity_type: &str) -> Option<&[Value]> {
        self.entities.get(entity_type).map(|v| v.as_slice())
    }

    /// Render a layout/container pattern's `children` into the current backend
    /// `ui`. `ui_ptr` is the live `ui`/`frame` type-erased via
    /// `ui as *mut _ as *mut ()`. No-op if the backend set no recursion hook.
    pub fn render_children(&mut self, ui_ptr: *mut (), props: &Value) {
        let Some(render) = self.child_render else {
            return;
        };
        // A `@trait.X` child switches the source trait for its subtree (so a button
        // inside it emits with source X); an inline child inherits the parent source.
        for (child, child_source) in
            crate::client::recurse::child_patterns(props, self.trait_content)
        {
            let prev = self.source_trait.clone();
            if child_source.is_some() {
                self.source_trait = child_source;
            }
            render(ui_ptr, &child, self);
            self.source_trait = prev;
        }
    }

    /// Render a single child pattern spec into the current backend `ui` — for a
    /// named node slot (e.g. `master`/`detail`). No-op if no recursion hook.
    pub fn render_one(&mut self, ui_ptr: *mut (), child: &Value) {
        if let Some(render) = self.child_render {
            render(ui_ptr, child, self);
        }
    }
}

/// A parsed render-ui slot item.
#[derive(Debug, Clone)]
pub struct SlotContent {
    /// Pattern name (e.g. "page-header").
    pub pattern: String,
    /// Pattern props from the server response.
    pub props: Value,
    /// Optional single entity context for the pattern.
    pub entity: Option<Value>,
    /// Trait whose `render-ui` produced this content — the event source for buttons
    /// at the top of this subtree (switched per `@trait.X` child as rendering descends).
    pub source_trait: Option<String>,
}

/// A shared empty per-trait content map for contexts without `@trait` support.
pub fn empty_trait_content() -> &'static ValueMap {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<ValueMap> = OnceLock::new();
    EMPTY.get_or_init(ValueMap::new)
}

/// Map an `@almadar/ui` spacing token (`none`..`2xl`) to egui pixels.
pub fn gap_to_px(gap: &str) -> f32 {
    match gap {
        "none" => 0.0,
        "xs" => 4.0,
        "sm" => 8.0,
        "md" => 12.0,
        "lg" => 16.0,
        "xl" => 24.0,
        "2xl" => 32.0,
        _ => 12.0,
    }
}

/// Extract a string property from pattern config
pub fn get_string(props: &Value, key: &str) -> Option<String> {
    props
        .as_object()
        .and_then(|o| o.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract a string with default value
pub fn get_string_or(props: &Value, key: &str, default: &str) -> String {
    get_string(props, key).unwrap_or_else(|| default.to_string())
}

/// Merge a renderer's base class with the optional `className` pattern prop.
///
/// `className` is a first-class pattern prop in `@almadar/core`
/// (pattern-types: `className?: string | SExpr`) carrying utility classes
/// (Tailwind) that the React substrate spreads onto the root element. Web
/// renderers (dioxus) honor the same contract: the prop appends to the
/// renderer's base class so utilities participate in the cascade. Non-web
/// backends (egui/terminal) never call this — they have no CSS.
pub fn merge_class_name(base: &str, props: &Value) -> String {
    let extra = get_string_or(props, "className", "");
    let extra = extra.trim();
    match (base.is_empty(), extra.is_empty()) {
        (true, true) => String::new(),
        (true, false) => extra.to_string(),
        (false, true) => base.to_string(),
        (false, false) => format!("{base} {extra}"),
    }
}

/// Extract a boolean property
pub fn get_bool(props: &Value, key: &str) -> Option<bool> {
    props
        .as_object()
        .and_then(|o| o.get(key))
        .and_then(|v| v.as_bool())
}

/// Extract a boolean with default
pub fn get_bool_or(props: &Value, key: &str, default: bool) -> bool {
    get_bool(props, key).unwrap_or(default)
}

/// Extract a number property
pub fn get_number(props: &Value, key: &str) -> Option<f64> {
    props
        .as_object()
        .and_then(|o| o.get(key))
        .and_then(|v| v.as_number())
}

/// Extract a number with default
pub fn get_number_or(props: &Value, key: &str, default: f64) -> f64 {
    get_number(props, key).unwrap_or(default)
}

/// Extract an array of strings
pub fn get_string_array(props: &Value, key: &str) -> Vec<String> {
    props
        .as_object()
        .and_then(|o| o.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract an array of values
pub fn get_array(props: &Value, key: &str) -> Vec<Value> {
    props
        .as_object()
        .and_then(|o| o.get(key))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// The explicit-rows fallback every backend shares: `rows` → `items` → `data`.
/// One owner for the precedence so egui / dioxus / terminal never drift.
pub fn get_explicit_rows(props: &Value) -> Vec<Value> {
    let mut r = get_array(props, "rows");
    if r.is_empty() {
        r = get_array(props, "items");
    }
    if r.is_empty() {
        r = get_array(props, "data");
    }
    r
}

/// `props` with every row-set marker replaced by the rows `resolve` answers for it, in the reference's order
/// (`resolve` is the kernel's one resolver: [`crate::client::ClientCore::resolve_rows`]). A reference that cannot be
/// resolved is replaced by an empty list, never by a stale copy.
pub fn resolve_row_set_markers(
    props: &Value,
    resolve: &dyn Fn(
        &orbital_core_base::row_set::RowSetRef,
    ) -> Result<crate::row_query::RowPage, String>,
) -> Value {
    if let Some(reference) = crate::row_query::as_row_set_marker(props) {
        return Value::Array(resolve(&reference).map(|page| page.rows).unwrap_or_default());
    }
    match props {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_row_set_markers(item, resolve))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), resolve_row_set_markers(v, resolve)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// HashMap-backed variant of [`get_entity_rows`] for backends whose render view
/// holds the entity cache as a plain map (the Dioxus backend's `DioxusView`).
/// Identical precedence: inline `entity` array → cache lookup by type name →
/// explicit `rows`/`items`/`data`.
pub fn get_entity_rows_from(props: &Value, entities: &HashMap<String, Vec<Value>>) -> Vec<Value> {
    if let Some(obj) = props.as_object() {
        match obj.get("entity") {
            Some(Value::Array(rows)) => return rows.clone(),
            Some(Value::String(entity)) if !entity.is_empty() => {
                if let Some(cached) = entities.get(entity) {
                    if !cached.is_empty() {
                        return cached.clone();
                    }
                }
            }
            _ => {}
        }
    }
    get_explicit_rows(props)
}

/// Resolve the row collection for a data pattern (`data-grid`/`table-view`/`entity-*`).
///
/// The `entity` prop is a resolved binding (the kernel runs every effect through
/// `BindingContext::resolve_value`), so it arrives as: (a) an inline array — the loaded
/// collection (`@payload.data`, `@entity.items`, …) → use it directly; or (b) a type-name
/// string → read the fetch cache by type. Falls back to explicit `rows`/`items`/`data`.
/// One owner for "where do a table's rows come from", instead of inlining it per renderer.
pub fn get_entity_rows(props: &Value, ctx: &RenderContext<'_>) -> Vec<Value> {
    if let Some(obj) = props.as_object() {
        match obj.get("entity") {
            Some(Value::Array(rows)) => return rows.clone(),
            Some(Value::String(entity)) if !entity.is_empty() => {
                if let Some(cached) = ctx.entity_data(entity) {
                    if !cached.is_empty() {
                        return cached.to_vec();
                    }
                }
            }
            _ => {}
        }
    }
    get_explicit_rows(props)
}

/// Resolve a dotted/bracket path (`a.b`, `items[0].id`) against a [`Value`],
/// mirroring the kernel's `@param(.field|[i])*` binding grammar. Returns `None`
/// if any segment misses — callers substitute `Value::Null` so an unresolved
/// row binding renders as the field's empty state rather than the raw token.
fn get_path(v: &Value, path: &str) -> Option<Value> {
    let mut cur = v;
    for seg in orbital_core_base::value::split_path(path) {
        let seg = seg.as_ref();
        let (name, idx) = match seg.find('[') {
            Some(i) if seg.ends_with(']') => {
                (&seg[..i], seg[i + 1..seg.len() - 1].parse::<usize>().ok())
            }
            _ => (seg, None),
        };
        cur = cur.as_object().and_then(|o| o.get(name))?;
        if let Some(i) = idx {
            cur = cur.as_array().and_then(|a| a.get(i))?;
        }
    }
    Some(cur.clone())
}

/// Structurally substitute `@<param>` / `@<param>.<path>` binding tokens in a
/// renderItem lambda body with values from `row`. The kernel leaves client-local
/// bindings (`@party.icon`) unresolved in the render-ui spec; the renderer binds
/// them per row here. Pure structural walk over [`Value`] — no evaluator —
/// covering the canonical data-list/data-grid renderItem shape. Non-matching
/// strings, numbers, bools and nulls pass through verbatim.
///
/// Shared by every backend: egui applies it inside `render_item_children`; the
/// Dioxus backend maps it per row and recurses through its registry.
pub fn substitute_row_bindings(spec: &Value, param: &str, row: &Value) -> Value {
    let prefix = format!("@{}", param);
    match spec {
        Value::String(s) => {
            let s = s.as_str();
            if s == prefix {
                return row.clone();
            }
            if let Some(rest) = s.strip_prefix(prefix.as_str()) {
                if let Some(path) = rest.strip_prefix('.') {
                    return get_path(row, path).unwrap_or(Value::Null);
                }
            }
            Value::String(s.to_string())
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|v| substitute_row_bindings(v, param, row)).collect())
        }
        Value::Object(obj) => {
            let mut out = ValueMap::with_capacity(obj.len());
            for (k, v) in obj {
                out.insert(k.clone(), substitute_row_bindings(v, param, row));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Extract `(param_name, body)` from a renderItem lambda carried by the
/// render-ui spec as the array literal `["fn", "<param>", <body>]`. `None` for
/// any other shape (the caller then falls back to its field-dump rendering).
/// Shared by every backend's data-list/data-grid implementation.
pub fn render_item_param_body(ri: &Value) -> Option<(String, &Value)> {
    let arr = ri.as_array()?;
    if arr.len() >= 3 && arr.first().and_then(|v| v.as_str()) == Some("fn") {
        let p = arr.get(1)?.as_str()?.to_string();
        return Some((p, &arr[2]));
    }
    None
}

/// Apply a `data-list` / `data-grid` `renderItem` lambda to every row: bind the
/// lambda's row param to each row (structural substitution) and render the
/// resulting child spec through the backend recursion hook. Returns `true` when
/// a renderItem was present and rendered, so the caller can skip its field-dump
/// fallback; `false` when there is no usable renderItem (caller falls back).
pub fn render_item_children(
    ui_ptr: *mut (),
    props: &Value,
    rows: &[Value],
    ctx: &mut RenderContext<'_>,
) -> bool {
    let Some(render) = ctx.child_render else {
        return false;
    };
    let Some(ri) = props.as_object().and_then(|o| o.get("renderItem")) else {
        return false;
    };
    let Some((param, body)) = render_item_param_body(ri) else {
        return false;
    };
    for row in rows {
        let child = substitute_row_bindings(body, &param, row);
        render(ui_ptr, &child, ctx);
    }
    true
}

/// Derive column-spec objects (`{"key":…, "label":…}`) for data-grid / table-view.
/// Resolution order: (a) `props["columns"]`; (b) `props["fields"]` (string or object
/// items); (c) the first non-empty row's object keys (skipping "id"). This is what stops
/// app tables rendering "No columns" when the `.orb` supplies `fields`, not `columns` —
/// the returned objects expose `key`/`label` so existing render loops work unchanged.
pub fn get_columns(props: &Value, rows: &[Value]) -> Vec<Value> {
    let spec = |key: &str, label: &str| -> Value {
        let mut o = ValueMap::new();
        o.insert("key".to_string(), Value::String(key.to_string()));
        o.insert("label".to_string(), Value::String(label.to_string()));
        Value::Object(o)
    };
    let from_arr = |arr: &[Value]| -> Vec<Value> {
        arr.iter()
            .filter_map(|c| {
                if let Some(s) = c.as_str() {
                    return Some(spec(s, &humanize_field_name(s)));
                }
                let o = c.as_object()?;
                let key = o
                    .get("key")
                    .or_else(|| o.get("field"))
                    .or_else(|| o.get("name"))
                    .and_then(|v| v.as_str())?;
                let label = o
                    .get("label")
                    .or_else(|| o.get("header"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| humanize_field_name(key));
                Some(spec(key, &label))
            })
            .collect()
    };
    for spec_key in ["columns", "fields"] {
        let arr = get_array(props, spec_key);
        if !arr.is_empty() {
            let cols = from_arr(&arr);
            if !cols.is_empty() {
                return cols;
            }
        }
    }
    for row in rows {
        if let Some(obj) = row.as_object() {
            let cols: Vec<Value> = obj
                .keys()
                .filter(|k| k.as_str() != "id")
                .map(|k| spec(k, &humanize_field_name(k)))
                .collect();
            if !cols.is_empty() {
                return cols;
            }
        }
    }
    Vec::new()
}

/// Action button definition extracted from pattern config
#[derive(Debug, Clone)]
pub struct ActionButton {
    /// Button label
    pub label: String,
    /// Event to fire on click
    pub event: String,
    /// Optional icon
    pub icon: Option<String>,
    /// Whether button is primary styled
    pub primary: bool,
    /// Whether button is danger styled
    pub danger: bool,
}

impl ActionButton {
    /// Create from a Value object
    pub fn from_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let label = obj.get("label")?.as_str()?;
        let event = obj.get("event")?.as_str()?;

        Some(Self {
            label: label.to_string(),
            event: event.to_string(),
            icon: obj
                .get("icon")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            primary: obj
                .get("primary")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            danger: obj.get("danger").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }
}

/// Extract action buttons from pattern config
pub fn get_actions(props: &Value, key: &str) -> Vec<ActionButton> {
    props
        .as_object()
        .and_then(|o| o.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(ActionButton::from_value).collect())
        .unwrap_or_default()
}

/// Form field definition extracted from pattern config
#[derive(Debug, Clone)]
pub struct FormField {
    /// Field name (maps to entity property)
    pub name: String,
    /// Display label
    pub label: String,
    /// Field type (string, number, boolean, enum, etc.)
    pub field_type: String,
    /// Whether field is required
    pub required: bool,
    /// Placeholder text
    pub placeholder: Option<String>,
    /// For enum types, the available options
    pub options: Vec<String>,
    /// Default value
    pub default: Option<Value>,
}

impl FormField {
    /// Create from a Value object
    pub fn from_value(value: &Value) -> Option<Self> {
        // A bare field name (render-ui emits `fields: ["name", ...]`) — no type
        // metadata, so default to a text input.
        if let Some(name) = value.as_str() {
            return Some(Self {
                name: name.to_string(),
                label: humanize_field_name(name),
                field_type: "string".to_string(),
                required: false,
                placeholder: None,
                options: Vec::new(),
                default: None,
            });
        }
        let obj = value.as_object()?;
        let name = obj.get("name")?.as_str()?;

        Some(Self {
            name: name.to_string(),
            label: obj
                .get("label")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| humanize_field_name(name)),
            field_type: obj
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "string".to_string()),
            required: obj
                .get("required")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            placeholder: obj
                .get("placeholder")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            options: obj
                .get("options")
                .or_else(|| obj.get("values"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            default: obj.get("default").cloned(),
        })
    }
}

/// Extract form fields from pattern config
pub fn get_fields(props: &Value, key: &str) -> Vec<FormField> {
    props
        .as_object()
        .and_then(|o| o.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(FormField::from_value).collect())
        .unwrap_or_default()
}

/// A HUD stat definition (`{source, field}` resolved against the entity cache).
/// Shared by the egui and Dioxus `game-hud` renderers — hoisted from
/// `rendering/game.rs` so both paint backends walk one schema.
#[derive(Debug, Clone)]
pub struct HudStat {
    pub source: String,
    pub field: String,
}

impl HudStat {
    pub fn from_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        Some(Self {
            source: obj.get("source")?.as_str()?.to_string(),
            field: obj.get("field")?.as_str()?.to_string(),
        })
    }
}

/// Extract HUD stats from pattern config, supporting `stats`, `items` and `elements` keys.
pub fn get_hud_stats(props: &Value) -> Vec<HudStat> {
    let mut stats = Vec::new();

    for key in ["stats", "items", "elements"] {
        for value in get_array(props, key) {
            if let Some(stat) = HudStat::from_value(&value) {
                stats.push(stat);
            } else if key == "elements" {
                // Support simple { type, bind } schema-style elements.
                if let Some(obj) = value.as_object() {
                    if let (Some(bind), Some(_ty)) =
                        (obj.get("bind").and_then(|v| v.as_str()), obj.get("type"))
                    {
                        let parts: Vec<&str> = bind.split('.').collect();
                        if parts.len() == 2 {
                            stats.push(HudStat {
                                source: parts[0].to_string(),
                                field: parts[1].to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    stats
}

/// Convert a field name to human-readable label
/// e.g., "firstName" -> "First Name", "user_email" -> "User Email"
pub fn humanize_field_name(name: &str) -> String {
    let mut result = String::new();
    let mut prev_lower = false;
    let mut after_separator = false;

    for (i, c) in name.chars().enumerate() {
        if c == '_' || c == '-' {
            result.push(' ');
            prev_lower = false;
            after_separator = true;
        } else if c.is_uppercase() && prev_lower {
            result.push(' ');
            result.push(c);
            prev_lower = false;
            after_separator = false;
        } else if i == 0 || after_separator {
            result.push(c.to_ascii_uppercase());
            prev_lower = c.is_lowercase();
            after_separator = false;
        } else {
            result.push(c);
            prev_lower = c.is_lowercase();
            after_separator = false;
        }
    }

    result
}

/// Extract value from entity by field name
pub fn get_entity_field(entity: &Value, field: &str) -> Option<Value> {
    entity.as_object().and_then(|o| o.get(field)).cloned()
}

/// Format a value for display
pub fn format_value(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::Bool(b) => if *b { "Yes" } else { "No" }.to_string(),
        Value::Number(n) => {
            if n.fract() == 0.0 {
                format!("{}", *n as i64)
            } else {
                format!("{:.2}", n)
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(arr) => format!("[{} items]", arr.len()),
        Value::Object(obj) => format!("{{{} fields}}", obj.len()),
        Value::Lambda { params, .. } => format!("[lambda({} params)]", params.len()),
    }
}

/// Inject entity data into pattern props
///
/// Used after receiving server response to populate render-ui patterns
/// with fetched entity data.
pub fn inject_entity_data(pattern: &mut Value, entity_type: &str, data: &[Value]) {
    if let Some(obj) = pattern.as_object_mut() {
        // Check if this pattern references the entity type
        if let Some(pattern_entity) = obj.get("entity").and_then(|v| v.as_str()) {
            if pattern_entity == entity_type {
                obj.insert("data".to_string(), Value::Array(data.to_vec()));
            }
        }
    }
}

/// Build a payload object from form data
pub fn build_form_payload(form_data: &HashMap<String, String>) -> Value {
    let mut payload = ValueMap::new();

    for (key, value) in form_data {
        // Try to parse as number or boolean
        let parsed_value = if value == "true" {
            Value::Bool(true)
        } else if value == "false" {
            Value::Bool(false)
        } else if let Ok(num) = value.parse::<f64>() {
            Value::Number(num)
        } else {
            Value::String(value.clone())
        };

        payload.insert(key.clone(), parsed_value);
    }

    Value::Object(payload)
}

/// Convert serde_json::Value to crate::Value
pub fn from_json(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(arr) => Value::Array(arr.into_iter().map(from_json).collect()),
        serde_json::Value::Object(obj) => {
            Value::Object(obj.into_iter().map(|(k, v)| (k, from_json(v))).collect())
        }
    }
}

/// Convert crate::Value to serde_json::Value
pub fn to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => serde_json::json!(*n),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Array(arr) => serde_json::Value::Array(arr.iter().map(to_json).collect()),
        Value::Object(obj) => {
            serde_json::Value::Object(obj.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
        Value::Lambda { params, .. } => {
            // Lambdas can't be serialized to JSON, represent as a string
            serde_json::Value::String(format!("[lambda({} params)]", params.len()))
        }
    }
}

/// Map a Lucide/Feather icon name to a legible Unicode glyph renderable by egui's
/// default font. Unknown names fall back to "•" instead of rendering as tofu (□).
pub fn icon_name_to_glyph(name: &str) -> &'static str {
    match name {
        "check" | "check-circle" | "check-square" | "success" => "✓",
        "x" | "x-circle" | "close" | "error" => "✕",
        "alert-triangle" | "warning" | "alert" | "shield-alert" => "⚠",
        "alert-octagon" => "⛔",
        "info" | "help-circle" => "ℹ",
        "shield" | "shield-check" | "lock" | "github" => "◆",
        "zap" | "lightning" => "⚡",
        "search" | "file-search" => "⌕",
        "home" => "⌂",
        "arrow-right" | "external-link" | "trending-up" | "share" => "↗",
        "arrow-left" | "trending-down" => "↙",
        "chevron-right" => "›",
        "chevron-left" => "‹",
        "chevron-up" => "↑",
        "chevron-down" => "↓",
        "repeat" | "refresh-ccw" | "history" | "rotate-cw" | "rotate-ccw" => "↺",
        "user" | "eye" | "bell" | "brain" | "gauge" | "life-buoy" => "◎",
        "user-plus" | "users" => "⊞",
        "mail" | "inbox" => "✉",
        "send" => "➤",
        "message-circle" | "message-square" => "◉",
        "megaphone" | "rss" | "video" | "monitor" | "camera" => "▶",
        "phone" => "☏",
        "file" | "file-text" | "file-signature" | "paperclip" | "copy" | "clipboard"
        | "clipboard-list" | "clipboard-check" | "layers" => "≡",
        "folder" => "▷",
        "list" | "list-ordered" | "list-checks" | "list-todo" | "book" | "book-open" => "☰",
        "database" | "server" | "hard-drive" | "cpu" | "save" | "download" | "image"
        | "package" | "truck" | "warehouse" | "ticket" | "printer" | "qr-code"
        | "id-card" | "briefcase" | "store" => "▣",
        "shopping-cart" | "shopping-bag" => "⊠",
        "clock" | "calendar" | "calendar-check" | "calendar-clock" | "calendar-range" => "⏱",
        "settings" | "gear" | "sliders" => "⚙",
        "layout-dashboard" | "layout-grid" | "layout-rows" | "kanban-square" => "▦",
        "map" | "map-pin" | "location" | "tag" | "bookmark" => "▾",
        "link" | "git-branch" | "git-pull-request" => "↱",
        "git-compare" => "↔",
        "terminal" => ">",
        "hash" => "#",
        "star" | "sparkles" => "★",
        "heart" => "♥",
        "flag" => "⚑",
        "credit-card" | "wallet" | "dollar-sign" | "receipt" => "$",
        "scale" => "⚖",
        "plus" | "plus-circle" | "add" => "+",
        "minus" => "−",
        "edit" | "pencil" => "✎",
        "trash-2" | "trash" => "✗",
        "crosshair" | "target" => "⊕",
        "rocket" => "↑",
        "pie-chart" | "bar-chart-2" => "◑",
        "wrench" | "tool" | "hammer" | "utensils" => "✦",
        "graduation-cap" | "chef-hat" | "hard-hat" => "▲",
        _ => "•",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create Value from JSON literal
    fn json_value(s: &str) -> Value {
        let json: serde_json::Value = serde_json::from_str(s).unwrap();
        from_json(json)
    }

    fn marker_props() -> Value {
        use orbital_core_base::row_set::{row_set_marker, RowSetRef};
        let mut props = json_value(r#"{"type":"table-view","title":"T"}"#);
        let reference = RowSetRef {
            entity_type: "Ticket".into(),
            ids: vec!["a".into(), "b".into()],
            total: 2,
            query: Default::default(),
        };
        props
            .as_object_mut()
            .unwrap()
            .insert("entity".into(), row_set_marker(&reference));
        props
    }

    #[test]
    fn a_row_set_marker_prop_is_replaced_by_the_rows_the_kernel_resolver_answers() {
        use crate::row_query::RowPage;
        let rows = vec![json_value(r#"{"id":"b"}"#)];
        let resolved = resolve_row_set_markers(&marker_props(), &|_| {
            Ok(RowPage { rows: rows.clone(), total: 1 })
        });
        assert_eq!(
            resolved.as_object().unwrap().get("entity"),
            Some(&Value::Array(rows))
        );
        assert_eq!(get_string(&resolved, "title"), Some("T".to_string()));
    }

    #[test]
    fn an_unresolvable_marker_is_an_empty_list_never_the_raw_marker_and_plain_props_pass_through() {
        let resolved = resolve_row_set_markers(&marker_props(), &|_| Err("host down".into()));
        assert_eq!(
            resolved.as_object().unwrap().get("entity"),
            Some(&Value::Array(Vec::new()))
        );
        let plain = json_value(r#"{"entity":[{"id":"a"}],"x":{"y":[1,2]}}"#);
        assert_eq!(resolve_row_set_markers(&plain, &|_| Err("unused".into())), plain);
    }

    #[test]
    fn test_get_string() {
        let props = json_value(r#"{"title": "Hello", "count": 42}"#);

        assert_eq!(get_string(&props, "title"), Some("Hello".to_string()));
        assert_eq!(get_string(&props, "missing"), None);
        assert_eq!(get_string(&props, "count"), None); // Not a string
    }

    #[test]
    fn test_get_string_array() {
        let props = json_value(r#"{"fields": ["name", "email", "status"]}"#);

        let fields = get_string_array(&props, "fields");
        assert_eq!(fields, vec!["name", "email", "status"]);
    }

    #[test]
    fn test_merge_class_name() {
        let with_cn = json_value(r#"{"className": "flex-1 hover:shadow-lg"}"#);
        let without_cn = json_value(r#"{"label": "x"}"#);

        assert_eq!(
            merge_class_name("almadar-card", &with_cn),
            "almadar-card flex-1 hover:shadow-lg"
        );
        assert_eq!(merge_class_name("almadar-card", &without_cn), "almadar-card");
        assert_eq!(merge_class_name("", &with_cn), "flex-1 hover:shadow-lg");
        assert_eq!(merge_class_name("", &without_cn), "");
    }

    #[test]
    fn test_action_button() {
        let action = json_value(r#"{"label": "Save", "event": "SAVE", "primary": true}"#);

        let button = ActionButton::from_value(&action).unwrap();
        assert_eq!(button.label, "Save");
        assert_eq!(button.event, "SAVE");
        assert!(button.primary);
    }

    #[test]
    fn test_get_actions() {
        let props = json_value(
            r#"{"actions": [{"label": "Edit", "event": "EDIT"}, {"label": "Delete", "event": "DELETE", "danger": true}]}"#,
        );

        let actions = get_actions(&props, "actions");
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].event, "EDIT");
        assert!(actions[1].danger);
    }

    #[test]
    fn test_humanize_field_name() {
        assert_eq!(humanize_field_name("firstName"), "First Name");
        assert_eq!(humanize_field_name("user_email"), "User Email");
        assert_eq!(humanize_field_name("id"), "Id");
        assert_eq!(humanize_field_name("connectionSlots"), "Connection Slots");
    }

    #[test]
    fn test_format_value() {
        assert_eq!(format_value(&Value::Null), "-");
        assert_eq!(format_value(&Value::Bool(true)), "Yes");
        assert_eq!(format_value(&Value::Number(42.0)), "42");
        assert_eq!(format_value(&Value::Number(2.5)), "2.50");
        assert_eq!(format_value(&Value::String("hello".to_string())), "hello");
    }

    #[test]
    fn test_inject_entity_data() {
        let mut pattern = json_value(r#"{"type": "data-grid", "entity": "User"}"#);

        let data = vec![
            json_value(r#"{"id": "1", "name": "Alice"}"#),
            json_value(r#"{"id": "2", "name": "Bob"}"#),
        ];

        inject_entity_data(&mut pattern, "User", &data);

        let injected = pattern.as_object().unwrap().get("data").unwrap();
        assert_eq!(injected.as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_parse_color_hex() {
        assert_eq!(parse_color_hex("#4678DC"), Some("#4678DC".to_string()));
        assert_eq!(parse_color_hex("#abc"), Some("#AABBCC".to_string()));
        assert_eq!(parse_color_hex("blue"), Some("#4678DC".to_string()));
        assert_eq!(parse_color_hex("grey"), Some("#808080".to_string()));
        // Placeholder junk from std sample data must NOT pass through.
        assert_eq!(parse_color_hex("Color"), None);
        assert_eq!(parse_color_hex(""), None);
        assert_eq!(parse_color_hex("#12"), None);
    }

    #[test]
    fn test_item_color_hex_falls_back_to_palette() {
        let good = json_value(r##"{"color": "#123456"}"##);
        assert_eq!(item_color_hex(&good, 3), "#123456");
        let junk = json_value(r#"{"color": "Color"}"#);
        assert_eq!(item_color_hex(&junk, 1), PALETTE_HEX[1]);
        let missing = json_value(r#"{"label": "x"}"#);
        assert_eq!(item_color_hex(&missing, 2), PALETTE_HEX[2]);
    }

    #[test]
    fn test_get_color_or() {
        let props = json_value(r#"{"stroke": "purple", "bad": "Color"}"#);
        assert_eq!(get_color_or(&props, "stroke", "#5B8DB8"), "#965AC8");
        assert_eq!(get_color_or(&props, "bad", "#5B8DB8"), "#5B8DB8");
        assert_eq!(get_color_or(&props, "missing", "#5B8DB8"), "#5B8DB8");
    }

    #[test]
    fn test_build_form_payload() {
        let mut form_data = HashMap::new();
        form_data.insert("name".to_string(), "Alice".to_string());
        form_data.insert("age".to_string(), "30".to_string());
        form_data.insert("active".to_string(), "true".to_string());

        let payload = build_form_payload(&form_data);
        let obj = payload.as_object().unwrap();

        assert_eq!(obj.get("name"), Some(&Value::String("Alice".to_string())));
        assert_eq!(obj.get("age"), Some(&Value::Number(30.0)));
        assert_eq!(obj.get("active"), Some(&Value::Bool(true)));
    }
}
