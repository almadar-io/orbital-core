//! Event contracts: call-site payloads, emits, listens and payload fields.

use super::*;

// =============================================================================
// Event Contracts
// =============================================================================

/// Binding grammar for a call-site-captured event payload reference.
///
/// When an inline trait REFERENCE's config value binds `@payload.<field>` at a
/// call site inside a composing render effect (e.g. JSX `<Typography
/// content={?error} />` where the composing transition's event declares
/// `error`), the lolo lowerer rewrites that value to
/// `@callsitePayload.<field>`. This distinct, source-tagged form means:
/// "resolve against the COMPOSING effect's triggering event payload at the call
/// site, then snapshot the literal into the child's config". It is deliberately
/// NOT `@payload.` so the child's own INIT scope never evaluates it — the
/// capture belongs to the parent transition's scope, not the child's.
pub const CALLSITE_PAYLOAD_PREFIX: &str = "@callsitePayload.";
/// The ordinary event-payload binding grammar the capture is rewritten FROM.
pub const PAYLOAD_PREFIX: &str = "@payload.";

/// If `s` is a call-site payload capture (`@callsitePayload.<path>`), return the
/// dotted `<path>` (the event-payload field path).
pub fn callsite_payload_path(s: &str) -> Option<&str> {
    s.strip_prefix(CALLSITE_PAYLOAD_PREFIX)
        .filter(|p| !p.is_empty())
}

/// Rewrite every `@payload.<path>` string inside a call-site config VALUE to the
/// call-site-captured form `@callsitePayload.<path>`. Applied ONLY to inline
/// trait-reference config values (the call-site override bucket), where a
/// `@payload` reference is, by position, a capture from the COMPOSING effect's
/// event — never the child atom's own payload.
pub fn capture_callsite_payload(expr: &mut SExpression) {
    match expr {
        SExpression::String(s) => {
            if let Some(rest) = s.strip_prefix(PAYLOAD_PREFIX) {
                *s = format!("{CALLSITE_PAYLOAD_PREFIX}{rest}").into();
            }
        }
        SExpression::Array(items) => items.iter_mut().for_each(capture_callsite_payload),
        SExpression::Object(map) => map.values_mut().for_each(capture_callsite_payload),
        SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => {}
    }
}

/// First path segment (the payload FIELD name) of a `@payload`/`@callsitePayload`
/// dotted path — `data` from `data`, `data[0]` from `data[0].x`, `row` from
/// `row.id`. The field is what an event's `payloadSchema` declares.
pub(super) fn payload_field_head(path: &str) -> &str {
    path.split(['.', '[']).next().unwrap_or(path)
}

/// Re-derive the payload-capture tag on every payload string in a call-site
/// config VALUE from actual scope evidence, rather than blanket-tagging by
/// position (`capture_callsite_payload`). For each `@payload.<field>` /
/// `@callsitePayload.<field>`, if `<field>` is in `local_fields` — the payload
/// fields the CHILD's own transition that renders this knob declares — the
/// binding resolves in the child's scope, so it is the ordinary `@payload.<path>`;
/// otherwise the field must come from the COMPOSING event, so it is the capture
/// `@callsitePayload.<path>`. Idempotent and source-agnostic (accepts either
/// incoming form), so a lower-time eager tag or a baked/untagged `.orb` both
/// converge to the correct tag. Fixes the over-approximation where overriding a
/// render-ui body knob (e.g. `galleryBodyContent`, rendered under the child's
/// own data-load transition) mis-tagged its `@payload.data` as a call-site
/// capture and validated it against the wrapper's events.
pub fn retag_callsite_payload(
    expr: &mut SExpression,
    local_fields: &std::collections::HashSet<String>,
) {
    match expr {
        SExpression::String(s) => {
            let rest = s
                .strip_prefix(PAYLOAD_PREFIX)
                .or_else(|| s.strip_prefix(CALLSITE_PAYLOAD_PREFIX));
            if let Some(rest) = rest {
                let is_local = local_fields.contains(payload_field_head(rest));
                *s = if is_local {
                    format!("{PAYLOAD_PREFIX}{rest}").into()
                } else {
                    format!("{CALLSITE_PAYLOAD_PREFIX}{rest}").into()
                };
            }
        }
        SExpression::Array(items) => items
            .iter_mut()
            .for_each(|i| retag_callsite_payload(i, local_fields)),
        SExpression::Object(map) => map
            .values_mut()
            .for_each(|v| retag_callsite_payload(v, local_fields)),
        SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => {}
    }
}

/// Collect the dotted field paths of every `@callsitePayload.<path>` string in
/// `expr`.
pub fn collect_callsite_payload_paths(expr: &SExpression, out: &mut Vec<String>) {
    match expr {
        SExpression::String(s) => {
            if let Some(p) = callsite_payload_path(s) {
                out.push(p.to_string());
            }
        }
        SExpression::Array(items) => items
            .iter()
            .for_each(|i| collect_callsite_payload_paths(i, out)),
        SExpression::Object(map) => map
            .values()
            .for_each(|v| collect_callsite_payload_paths(v, out)),
        SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => {}
    }
}

/// True when a config value IS or CONTAINS a call-site payload capture. The
/// inline phase uses this to keep the bound knob UNSUBSTITUTED in the child
/// body (leaving `@config.<knob>`), so each engine resolves the capture against
/// the composing effect's payload at the call site and passes the literal into
/// the child's config.
pub fn contains_callsite_payload(expr: &SExpression) -> bool {
    let mut v = Vec::new();
    collect_callsite_payload_paths(expr, &mut v);
    !v.is_empty()
}

/// `@config.<knob>` is the only legal reference form for an emit/listen event
/// name (Option B — the declared event name resolves to a string-typed
/// config knob's value at inline time). Returns the knob identifier when
/// `event` matches that form; `None` for a plain literal event name.
pub fn config_ref_event_knob(event: &str) -> Option<&str> {
    let knob = event.strip_prefix("@config.")?;
    if knob.is_empty() || knob.contains('.') {
        return None;
    }
    Some(knob)
}

/// The set of a trait's config knobs wired in a DEFINER position — named by
/// `emits { @config.<knob> }`, a stateMachine `events[].key` sentinel, or a
/// transition `event` sentinel. A definer knob's VALUE CREATES the event
/// name (A-UNIFY); it is exempt from event-set membership checks, unlike a
/// reference knob whose value must name an already-declared event. Single
/// owner of this rule — `orbital-compiler`'s `trait_event_ctx` (definer half)
/// and `stamp_config_ref_ids` (refId-stamping skip) both call through here
/// rather than recomputing it, since `orbital-core` is the shared ancestor of
/// both call sites and this is trait-declaration-only (no cross-orbital
/// resolution).
pub fn definer_knobs_for_trait(trait_def: &TraitDefinition) -> std::collections::HashSet<String> {
    let mut definer_knobs: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(emits) = &trait_def.emits {
        for emit in emits {
            if let Some(knob) = config_ref_event_knob(&emit.event) {
                definer_knobs.insert(knob.to_string());
            }
        }
    }
    if let Some(events) = &trait_def.state_machine.events {
        for event in events {
            if let Some(knob) = config_ref_event_knob(&event.key) {
                definer_knobs.insert(knob.to_string());
            }
        }
    }
    for transition in &trait_def.state_machine.transitions {
        if let Some(knob) = config_ref_event_knob(&transition.event) {
            definer_knobs.insert(knob.to_string());
        }
    }
    definer_knobs
}

/// Why a `@config.<knob>` emit/listen event-name reference failed to
/// resolve, per the Option B contract: the knob must be a DECLARED
/// string-typed config field with a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRefEventError {
    UnknownKnob,
    NotString,
    NoDefault,
}

/// Resolve a `@config.<knob>` emit/listen event-name reference to its
/// concrete literal. `effective_config` is the call-site-override-over-
/// declared-default map (same precedence `apply_overrides_to_trait` already
/// builds for `@config.X` body substitution); `declared_config` is the
/// trait's own `config {}` schema, used to check the string/event+default
/// contract. The single owner of this resolution rule — both the Rust
/// inline phase and the JS runtime reference-resolver call through the
/// equivalent contract (`resolveConfigRefEventName` in `@almadar/core`).
/// A-UNIFY: definer knobs spell `: event` same as reference knobs — the
/// VALUE must still be a plain string literal (checked below), only the
/// declared field TYPE is now `"string" | "event"`.
pub fn resolve_config_ref_event_name(
    event: &str,
    effective_config: &crate::schema::sexpr::SObject,
    declared_config: Option<&crate::collections::IndexMap<String, ConfigField>>,
) -> Result<String, ConfigRefEventError> {
    let knob = config_ref_event_knob(event).ok_or(ConfigRefEventError::UnknownKnob)?;
    let field = declared_config
        .and_then(|d| d.get(knob))
        .ok_or(ConfigRefEventError::UnknownKnob)?;
    if field.field_type != "string" && field.field_type != "event" {
        return Err(ConfigRefEventError::NotString);
    }
    if field.default.is_none() {
        return Err(ConfigRefEventError::NoDefault);
    }
    match effective_config.get(knob) {
        Some(SExpression::String(s)) => Ok(s.clone().into_inner()),
        _ => Err(ConfigRefEventError::NotString),
    }
}

/// Event emission definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmitDefinition {
    /// Event name
    pub event: String,

    /// V4 dual-carry id sibling of `event` — optional until the Phase-7 flip.
    #[serde(rename = "eventId", default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,

    /// Event description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// User-vocabulary synonyms (comma-separated) for the event matcher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,

    /// Authoring tier (`essential`/`customization`/`advanced`/`internal`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,

    /// Event scope
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<EventScope>,

    /// Payload schema — declarative type info for the event's payload.
    /// JSON key: `payloadSchema`. Distinct from the runtime payload value
    /// (`@payload.X` bindings, `EventPayload`) which is unrelated.
    #[serde(rename = "payloadSchema", skip_serializing_if = "Option::is_none")]
    pub payload_schema: Option<Vec<PayloadField>>,

    /// The config knob whose VALUE produced `event`, when this emit was
    /// declared as `emits { @config.<knob> -> … }`. Set by the inline phase,
    /// which resolves the reference to a literal and — before this field —
    /// discarded the fact that it ever was one.
    ///
    /// Why it matters: an atom's definer-knob emit is an OFFER (a DataGrid
    /// exposes `SORT`/`LOAD_MORE`; an Alert exposes `DISMISS`), and a consumer
    /// declining to wire it is an unsorted grid, not a defect. Without this,
    /// an unwired offer is indistinguishable from an authored emit that lost
    /// its listener, so every offer had to be reported.
    ///
    /// `None` = the event name was authored literally, which IS an obligation.
    #[serde(
        rename = "definerKnob",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub definer_knob: Option<String>,

    /// `Some(true)` when `scope` was set by a CALL-SITE `emitsScope` override
    /// (the inline phase applies it), not by the source atom's own `->
    /// external` declaration. The distinction carries consequence: an atom
    /// declaring its emits external is an unexamined default (nearly every
    /// atom does), while a composition restating it is the consumer's own
    /// deferral — "my consumer wires this" — which the dead-control check
    /// honours as an exemption. See ORB-X-EXTERNAL-EMIT-EXEMPTION.
    #[serde(
        rename = "scopeOverridden",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub scope_overridden: Option<bool>,

    /// The entity this WHOLE payload was declared as (`type X = Event
    /// <EntityName>`, `Event @entity`, or `Event E`) — set by `orbital-lolo` at
    /// lowering, when the alias's payload resolves to a known entity's own
    /// field list flattened at the payload root (G4,
    /// `Almadar_Compiler_Gaps.md` §82). Distinct from `PayloadField.entity`
    /// (a single NESTED field's value is an entity instance): this marks
    /// the payload schema ITSELF as a 1:1 mirror of that entity's CURRENT
    /// fields. Consumed by `resolve_type_param_sentinels`
    /// (`phases/inline/rewrite.rs`) to re-derive `payload_schema` from the
    /// entity's post-`fields{}`/`extend{}` shape once every orbital import
    /// or trait-reference override has materialized; kept in sync across
    /// entity renames by `rewrite_payload_entity_refs`, the same one owner
    /// `PayloadField.entity` already uses. `None` for any other payload
    /// shape (authored wrapper, struct, primitive or array). Symbolic `@entity`
    /// and `$E` origins remain symbolic for implicit linked-entity binding;
    /// explicit entity arguments become concrete, and scalar/struct arguments clear it.
    /// Old unmarked generic carriers must be regenerated from source; their
    /// `value` field name cannot distinguish them from authored wrappers.
    #[serde(
        rename = "payloadEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub payload_entity: Option<String>,
}

/// Event listener definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListenDefinition {
    /// Event to listen for (bare event name once `source` is populated;
    /// legacy concatenated form `"Source.EVENT"` when older .orb files are loaded)
    pub event: String,

    /// V4 dual-carry id sibling of `event` — optional until the Phase-7 flip.
    #[serde(rename = "eventId", default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,

    /// Internal event to trigger (defaults to same as event if not specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggers: Option<String>,

    /// V4 dual-carry id sibling of `triggers` — optional until the Phase-7 flip.
    #[serde(
        rename = "triggersId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub triggers_id: Option<String>,

    /// Event scope
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<EventScope>,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// User-vocabulary synonyms (comma-separated) for the event matcher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,

    /// Authoring tier (`essential`/`customization`/`advanced`/`internal`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,

    /// Payload mapping
    #[serde(rename = "payloadMapping", skip_serializing_if = "Option::is_none")]
    pub payload_mapping: Option<crate::schema::sexpr::SObject>,

    /// Guard condition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<SExpression>,

    /// `per <expr>`: the row of the listener's entity this delivery steps (EXEC-STEP-1a). Evaluated against the raw
    /// source payload when the delivery is processed; it must name a non-empty string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<SExpression>,

    /// Source-scope descriptor parsed from the `.lolo` authoring layer.
    /// See the runtime-side `ListenSourceDescriptor` for the matching shape.
    /// `None` subscribes without a source restriction; local payload declarations do not lower here.
    #[serde(rename = "source", skip_serializing_if = "Option::is_none")]
    pub source: Option<ListenSource>,
}

/// Source-scoping for a listen entry.
///
/// Mirrors the TypeScript `ListenSource` in `@almadar/core`. The authoring
/// forms are:
/// - `* EVENT -> TRIGGER`             → `ListenSource::Any`
/// - `TraitName EVENT -> TRIGGER`     → `ListenSource::Trait { trait }`
/// - `Orbital.Trait EVENT -> TRIGGER` → `ListenSource::Orbital { orbital, trait }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ListenSource {
    Any,
    Trait {
        #[serde(rename = "trait")]
        trait_name: String,
        /// V4 dual-carry id sibling of `trait` — optional until the Phase-7 flip.
        #[serde(rename = "traitId", default, skip_serializing_if = "Option::is_none")]
        trait_id: Option<String>,
    },
    Orbital {
        orbital: String,
        /// V4 dual-carry id sibling of `orbital` — optional until the Phase-7 flip.
        #[serde(rename = "orbitalId", default, skip_serializing_if = "Option::is_none")]
        orbital_id: Option<String>,
        #[serde(rename = "trait")]
        trait_name: String,
        /// V4 dual-carry id sibling of `trait` — optional until the Phase-7 flip.
        #[serde(rename = "traitId", default, skip_serializing_if = "Option::is_none")]
        trait_id: Option<String>,
    },
}

/// Split a `listens`-addressed event (`EVENT`, `Trait.EVENT`, `*.EVENT`,
/// `Orbital.Trait.EVENT`) into the bare event and its source. The one parser
/// of the address form, shared by `listens` lowering and event-typed values.
pub fn split_event_address(raw: &str) -> (String, Option<ListenSource>) {
    let parts: Vec<&str> = raw.split('.').collect();
    match parts.len() {
        0 | 1 => (raw.to_string(), None),
        2 => {
            let (src, ev) = (parts[0], parts[1]);
            if src == "*" {
                (ev.to_string(), Some(ListenSource::Any))
            } else {
                (
                    ev.to_string(),
                    Some(ListenSource::Trait {
                        trait_name: src.to_string(),
                        trait_id: None,
                    }),
                )
            }
        }
        n => (
            parts[n - 1].to_string(),
            Some(ListenSource::Orbital {
                orbital: parts[..n - 2].join("."),
                trait_name: parts[n - 2].to_string(),
                orbital_id: None,
                trait_id: None,
            }),
        ),
    }
}

/// Inverse of [`split_event_address`].
pub fn join_event_address(event: &str, source: Option<&ListenSource>) -> String {
    match source {
        None => event.to_string(),
        Some(ListenSource::Any) => format!("*.{event}"),
        Some(ListenSource::Trait { trait_name, .. }) => format!("{trait_name}.{event}"),
        Some(ListenSource::Orbital {
            orbital,
            trait_name,
            ..
        }) => format!("{orbital}.{trait_name}.{event}"),
    }
}

/// Event scope
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EventScope {
    Internal,
    External,
}

/// One condition of a payload field's `type_when` conditional list: "when
/// config knob `knob` resolves to the literal whose string spelling is
/// `equals`, this field's type is `ty`". Declared DATA — authored in
/// `.lolo` as `@typeWhen "<knob>=<literal>-><type>"` — never a compiler-side
/// predicate over a specific knob name. See `resolve_payload_type_when`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PayloadTypeWhen {
    pub knob: String,
    pub equals: String,
    #[serde(rename = "type")]
    pub ty: String,
}

/// Payload field definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PayloadField {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    #[serde(default, rename = "additionalProperties", skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<Box<PayloadField>>,
    /// The field's name; empty for a list's or map's element (`items`), which has none.
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    /// Closed value set (`"a" | "b"`), as on an entity field or a config knob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Nested object shape for an anonymous struct payload field (`field_type`
    /// = `"object"`). Set when the field is an inline `{ … }` object or a
    /// `type X = { … }` struct alias, so codegen emits a strict inline object
    /// type instead of the shapeless `EventPayload`. Entity-referencing fields
    /// keep the flat dotted-flatten convention and leave this `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<PayloadField>>,
    /// Declared knob-conditional type list (`@typeWhen` in `.lolo`). Each
    /// condition is checked in order against the trait's DECLARED config
    /// (resolved literal); the first match's `ty` is this field's real type
    /// at that call site. Resolved and CLEARED by
    /// `resolve_payload_type_when`'s callers once composed — the emitted
    /// `.orb` carries the resolved truth, not the conditional.
    #[serde(rename = "typeWhen", skip_serializing_if = "Option::is_none")]
    pub type_when: Option<Vec<PayloadTypeWhen>>,
    /// The declared entity this field's value IS an instance of — stamped by
    /// lowering when an entity-typed payload field (`row: Note`, scalar or
    /// `[Note]` array — the array form stamps the element entity) is
    /// flattened into `field_type: "object"`/`"[object]"` + `properties`.
    /// `None` for an anonymous struct/object field with no declared entity.
    /// Never inferred from shape or name downstream — this is the only
    /// source of entity identity for a payload field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// The declared field this payload field's value — each element's, for an
    /// array type — was projected from: `.lolo` `T.f` / `[T.f]`
    /// (`tagId : NoteTagRef.id`); see [`FieldDefinition::projected_from`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "projectedFrom"
    )]
    pub projected_from: Option<FieldProjection>,
    /// The element type of an `array` field whose element is itself a typed field (a union of named types:
    /// `[(Adv | string)]`), the same nested form an entity field's `items` takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<PayloadField>>,
}

/// The canonical string spelling of a resolved config literal, for comparison
/// against a `PayloadTypeWhen::equals` string. A `String` compares by its own
/// content; `Bool`/`Number` compare by their canonical Rust `Display`
/// spelling (`"true"`/`"false"`, integers without a trailing `.0`). Any other
/// shape (array/object/null) has no literal spelling and never matches.
pub(super) fn config_literal_spelling(value: &SExpression) -> Option<String> {
    match value {
        SExpression::String(s) => Some(s.clone().into_inner()),
        SExpression::Bool(b) => Some(b.to_string()),
        SExpression::Number(n) if n.fract() == 0.0 && n.abs() < i64::MAX as f64 => {
            Some((*n as i64).to_string())
        }
        SExpression::Number(n) => Some(n.to_string()),
        SExpression::Array(_) | SExpression::Object(_) | SExpression::Null => None,
    }
}

/// Resolve a payload field's `type_when` conditional list to a concrete type,
/// given the trait's DECLARED config (`config[knob].default` — by the time
/// either caller reads it, post-inline or post-validation-inline, this
/// already carries the call site's resolved override; see
/// `apply_overrides_to_trait`'s GAP-AG-VALUE-DRIFT fold). First condition
/// whose `knob` resolves to a literal spelled exactly `equals` wins. A
/// forwarded/unresolved knob (absent, non-literal, or not declared) matches
/// nothing — same safety semantics as the narrowing this replaces. Carries
/// ZERO knob-name or pattern-name knowledge: `conditions` is authored data,
/// this only evaluates it.
pub fn resolve_payload_type_when(
    conditions: &[PayloadTypeWhen],
    declared_config: Option<&crate::collections::IndexMap<String, ConfigField>>,
) -> Option<String> {
    conditions.iter().find_map(|cond| {
        let resolved = declared_config?.get(&cond.knob)?.default.as_ref()?;
        (config_literal_spelling(resolved).as_deref() == Some(cond.equals.as_str()))
            .then(|| cond.ty.clone())
    })
}
