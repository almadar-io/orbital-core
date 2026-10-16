//! Cross-backend `OirType` → target-type rendering.
//!
//! Every language shell used to hand-write the same recursive `match` over
//! [`OirType`] to turn an IR type into a target-language type. Those walks
//! drift independently and every new `OirType` variant means editing every
//! backend and hoping none is missed. This module owns the traversal once:
//! [`render_type`] walks the tree and each backend supplies only the leaves
//! and the "combine already-rendered children" steps via [`TargetTypeRenderer`].
//! Adding a variant here forces every backend to make an explicit,
//! compiler-checked decision — the structural anti-drift property.
//!
//! Backends live in sibling crates (`orbital-shell-*`); like the [`Backend`]
//! trait (`crate::generate`) and [`type_compat`](crate::type_compat), this
//! sits in `orbital-core` so shells implement it without depending on the
//! compiler.

use super::{OirField, OirType};

/// How faithfully a backend's rendered target type carries an [`OirType`]'s
/// structure and semantics — the Phase-1 faithfulness classification of
/// `docs/Almadar_Compiler_Spec.md` (per `docs/Orbital_Type_Chain_Table.md`).
/// Metadata only: it never influences a rendered byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeMappingClass {
    /// The target type carries the full structure/semantics.
    Faithful,
    /// A supertype that accepts all legal values but loses precision
    /// (Int→Double, a semantic string-domain→String, Money→number).
    Widening,
    /// Structure erased into a shapeless carrier (`json()`, `AnyCodable`,
    /// `JsonElement`, `serde_json::Value`, `Dict[str, Any]`).
    Lossy,
}

impl TypeMappingClass {
    /// The weaker of two classes — how classifiers combine children (e.g. a
    /// map is at best as faithful as its least faithful component).
    pub fn meet(self, other: Self) -> Self {
        use TypeMappingClass::*;
        match (self, other) {
            (Lossy, _) | (_, Lossy) => Lossy,
            (Widening, _) | (_, Widening) => Widening,
            _ => Faithful,
        }
    }
}

/// The canonical struct shape of a `file`-typed value at rest:
/// `{ name, url, mimeType, sizeBytes }`. `url` is a `data:` URI in
/// mock/playground mode or an object-storage URL in production. ONE owner
/// for the shape — the default [`TargetTypeRenderer::file`] rendering, the
/// type-compat validity rule, and the TS `FileValueSchema` all mirror it.
pub fn file_value_fields() -> Vec<OirField> {
    vec![
        OirField::new("name", OirType::String),
        OirField::new("url", OirType::String),
        OirField::new("mimeType", OirType::String),
        OirField::new("sizeBytes", OirType::Number),
    ]
}

/// Renders each [`OirType`] leaf and combines already-rendered children into a
/// backend's target type. The recursion is owned by [`render_type`]; an
/// implementor only decides how each node maps.
///
/// Context that is constant across a single walk (TS mode/catalog, Rust
/// entity/field names, mobile context name) belongs as fields on the
/// implementor (`self`), not as threaded parameters. The only per-descent
/// variation is the `enum_values` sidecar, which lives on [`OirField`] (not on
/// [`OirType::Enum`]) and applies only at the immediate field position — the
/// walker forces it to `None` on every recursion.
pub trait TargetTypeRenderer {
    /// The rendered output. `String` for TS/Python/Kotlin/Swift; a
    /// `proc_macro2::TokenStream` for the Rust backend (kept as an associated
    /// type so Rust need not stringify-and-reparse, which would perturb
    /// whitespace).
    type Out;

    // --- leaves ---
    /// `enum_values` is `Some` when a format-union field lowered to `String`
    /// plus the sidecar; the backend renders a union instead of a bare string.
    fn string(&self, enum_values: Option<&[String]>) -> Self::Out;
    fn number(&self) -> Self::Out;
    fn integer(&self) -> Self::Out;
    fn boolean(&self) -> Self::Out;
    fn date(&self) -> Self::Out;
    /// Every backend treats `DateTime` like `Date` today; override if that ever
    /// diverges.
    fn datetime(&self) -> Self::Out {
        self.date()
    }
    /// The semantic string domains ARE strings in every target language, so they
    /// default to the string rendering. Overriding is only needed if a target
    /// ever grows a native type for one of them. This default is what lets the
    /// python/rust/android/ios/mobile backends compile untouched.
    fn email(&self) -> Self::Out {
        self.string(None)
    }
    fn url(&self) -> Self::Out {
        self.string(None)
    }
    fn phone(&self) -> Self::Out {
        self.string(None)
    }
    fn uuid(&self) -> Self::Out {
        self.string(None)
    }
    fn image(&self) -> Self::Out {
        self.string(None)
    }
    /// Binary value — a base64 string at rest in every target language.
    fn bytes(&self) -> Self::Out {
        self.string(None)
    }
    /// Monetary amount — numeric at rest in every target language; the type
    /// exists so display surfaces can dispatch currency formatting. Override
    /// only if a target grows a native money type.
    fn money(&self) -> Self::Out {
        self.number()
    }
    /// Uploaded file/attachment — the canonical struct value
    /// `{ name, url, mimeType, sizeBytes }`. Defaults to the target's struct
    /// rendering of [`file_value_fields`]; override for a named type.
    fn file(&self) -> Self::Out {
        self.struct_ty(&file_value_fields())
    }
    fn json(&self) -> Self::Out;
    /// Renderable UI content (`node`). Defaults to the target's `json`
    /// rendering — only targets with a canonical pattern-tree type (the TS
    /// shell's `PatternValue`) override.
    fn node(&self) -> Self::Out {
        self.json()
    }
    /// A declared relation field stores the referenced row's id; the entity's
    /// shape is erased. Shells with a payload/inferred position override this.
    fn reference(&self, _entity: &str) -> Self::Out {
        self.string(None)
    }
    /// An embedded full row of the entity (`@intrinsic` list-of-entity
    /// field). No default: every shell decides how it spells the entity's
    /// struct.
    fn row(&self, entity: &str) -> Self::Out;
    fn enum_ty(&self, name: &str, enum_values: Option<&[String]>) -> Self::Out;
    fn trait_ref(&self) -> Self::Out;
    /// An orbital value held as data. Defaults to the target's `json` rendering.
    fn orbital_value(&self) -> Self::Out {
        self.json()
    }
    /// A whole behavior held as data. Defaults to the target's `json` rendering.
    fn behavior_value(&self) -> Self::Out {
        self.json()
    }
    fn sexpr(&self) -> Self::Out;
    /// A by-name reference to a named struct type (a recursive back-reference
    /// the resolver stopped expanding, or a Phase-3D nominalized anonymous
    /// shape). Defaults to the target's `json` rendering — wire-safe for a
    /// backend without a named-type facility; shells with one (TS from the
    /// start, Kotlin/Swift/Python/Rust since the Phase-3D readiness flip)
    /// override to emit the bare name and hoist the declaration from
    /// `named_types`.
    fn type_ref(&self, _name: &str) -> Self::Out {
        self.json()
    }

    // --- combine already-rendered children ---
    // The raw child `&OirType` is passed alongside its rendered form so a
    // backend can gate on the child shape (e.g. Rust collapses non-`String`
    // map keys to `serde_json::Value`, ignoring the rendered key).
    fn array(&self, inner_ty: &OirType, inner: Self::Out) -> Self::Out;
    /// A fixed-length tuple. Targets without tuples render a homogeneous tuple as an
    /// array of its slot type and a mixed one as an array of `json`.
    fn tuple(&self, items_ty: &[OirType], items: Vec<Self::Out>) -> Self::Out {
        match items_ty.first() {
            Some(first) if items_ty.iter().all(|t| t == first) => match items.into_iter().next() {
                Some(out) => self.array(first, out),
                None => self.array(&OirType::Json, self.json()),
            },
            _ => self.array(&OirType::Json, self.json()),
        }
    }
    fn map(
        &self,
        key_ty: &OirType,
        key: Self::Out,
        value_ty: &OirType,
        value: Self::Out,
    ) -> Self::Out;
    fn optional(&self, inner_ty: &OirType, inner: Self::Out) -> Self::Out;

    /// `Struct` keeps field iteration on the backend: each field carries its own
    /// `enum_values` sidecar, so the backend calls [`render_type`] per field
    /// with that field's sidecar (or widens to a catch-all object type).
    fn struct_ty(&self, fields: &[OirField]) -> Self::Out;
    /// A tagged union of struct variants. Defaults to the target's `json`
    /// rendering so backends that cannot spell a union compile untouched (the
    /// same posture as the semantic string domains); shells with
    /// a real union facility override both render and classification in the
    /// same change — TS (discriminated union), Kotlin (sealed class),
    /// Swift (associated-value enum), Python (`typing.Union` of named
    /// models), Rust (`#[serde(untagged)]` enum over named structs).
    fn union_ty(&self, _variants: &[OirField]) -> Self::Out {
        self.json()
    }

    // --- faithfulness classification ---
    // One `classify_*` method per render method above. Classification lives
    // HERE (not in a parallel walk) so the two can never drift structurally:
    // [`classify_type`] dispatches the identical `match` as [`render_type`],
    // and every required render leaf has a required classify twin — a new
    // `OirType` variant or a new arm forces a compile-time decision on BOTH.
    // The required twins below carry no default: a default would let a shell
    // override the rendering while silently inheriting a classification that
    // no longer describes it. The provided twins mirror the provided render
    // defaults exactly, and each pair's doc comment states the one shared
    // decision for that arm.
    //
    // Shared decisions worth pinning (the table's judgement calls):
    // - The semantic string domains (`email`…`image`) RENDER via `string(None)`
    //   but CLASSIFY as `Widening`: the carrier is the faithful string arm,
    //   the erased semantic domain is the loss.
    // - `money` RENDERS via `number()` but CLASSIFIES as `Widening` (currency
    //   semantics erased), even where `number` itself is `Faithful`.
    // - Arms defaulting to `json()` classify as the classification of `json`
    //   (usually `Lossy`); a shell overriding the rendering must override the
    //   classification in the same commit (TS `node`/`type_ref`/
    //   `union_ty` are the worked example).

    /// Class of [`TargetTypeRenderer::string`]. `enum_values: Some` (a
    /// format-union sidecar) is Faithful in every shell — the union spells the
    /// closed domain.
    fn classify_string(&self, enum_values: Option<&[String]>) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::number`].
    fn classify_number(&self) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::integer`].
    fn classify_integer(&self) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::boolean`].
    fn classify_boolean(&self) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::date`].
    fn classify_date(&self) -> TypeMappingClass;
    /// Pair decision: renders exactly like `date` in every backend today, so
    /// it classifies exactly like `date` — override both together if a target
    /// ever grows a native distinction.
    fn classify_datetime(&self) -> TypeMappingClass {
        self.classify_date()
    }
    /// Pair decision: renders via `string(None)` (carrier faithful) but the
    /// semantic domain is erased — `Widening`, in every shell, by default.
    /// Override together with `email()` if a target grows a native type.
    fn classify_email(&self) -> TypeMappingClass {
        let _ = self;
        TypeMappingClass::Widening
    }
    /// Same shared decision as [`TargetTypeRenderer::classify_email`].
    fn classify_url(&self) -> TypeMappingClass {
        let _ = self;
        TypeMappingClass::Widening
    }
    /// Same shared decision as [`TargetTypeRenderer::classify_email`].
    fn classify_phone(&self) -> TypeMappingClass {
        let _ = self;
        TypeMappingClass::Widening
    }
    /// Same shared decision as [`TargetTypeRenderer::classify_email`].
    fn classify_uuid(&self) -> TypeMappingClass {
        let _ = self;
        TypeMappingClass::Widening
    }
    /// Same shared decision as [`TargetTypeRenderer::classify_email`].
    fn classify_image(&self) -> TypeMappingClass {
        let _ = self;
        TypeMappingClass::Widening
    }
    /// Same shared decision as [`TargetTypeRenderer::classify_email`]: the
    /// binary meaning is erased to base64 text.
    fn classify_bytes(&self) -> TypeMappingClass {
        let _ = self;
        TypeMappingClass::Widening
    }
    /// Pair decision: renders via `number()` but the currency semantics of a
    /// monetary amount are erased — `Widening` even where `number` is
    /// `Faithful` (table row 12 in every shell).
    fn classify_money(&self) -> TypeMappingClass {
        let _ = self;
        TypeMappingClass::Widening
    }
    /// Pair decision: renders as the target's `struct_ty` of
    /// [`file_value_fields`], so it classifies as that same struct arm.
    fn classify_file(&self) -> TypeMappingClass {
        self.classify_struct_ty(&file_value_fields())
    }
    /// Class of [`TargetTypeRenderer::json`].
    fn classify_json(&self) -> TypeMappingClass;
    /// Pair decision: the default renders via `json()`, which preserves the
    /// pattern tree's STRUCTURE and erases only the pattern TYPE — the shells
    /// keeping the json carrier classify this `Widening` (structure kept,
    /// type precision lost), NOT the `json` arm's class. Typed `PatternNode`
    /// codegen from the baked registry — the faithful end-state — is
    /// deferred (Phase-3 follow-up, rides on named_types); until it lands,
    /// only TS (the canonical `PatternValue`) overrides the rendering and
    /// classifies `Faithful`.
    fn classify_node(&self) -> TypeMappingClass {
        self.classify_json()
    }
    /// Pair decision: the default renders via `string(None)` (id carrier), so
    /// the default classifies `Widening` — the entity shape is erased.
    fn classify_reference(&self, _entity: &str) -> TypeMappingClass {
        TypeMappingClass::Widening
    }
    /// Class of [`TargetTypeRenderer::row`]. No default: the shell that
    /// renders the row decides how faithfully.
    fn classify_row(&self, entity: &str) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::enum_ty`].
    fn classify_enum_ty(&self, name: &str, enum_values: Option<&[String]>) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::trait_ref`].
    fn classify_trait_ref(&self) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::orbital_value`].
    fn classify_orbital_value(&self) -> TypeMappingClass {
        self.classify_json()
    }
    /// Class of [`TargetTypeRenderer::behavior_value`].
    fn classify_behavior_value(&self) -> TypeMappingClass {
        self.classify_json()
    }
    /// Class of [`TargetTypeRenderer::sexpr`]. The json-carrier shells record
    /// this as a PERMANENT deliberate `Lossy`: s-expressions surface only in
    /// config/knob positions, never as entity data, so the erasure has no
    /// at-rest consequence — pinned here so the row stops resurfacing
    /// (gaps §130 item 4). TS spells the real `SExpr` type and classifies
    /// `Faithful`.
    fn classify_sexpr(&self) -> TypeMappingClass;
    /// Pair decision: the default renders via `json()`, so the default
    /// classifies as `json()`; shells with a named-type facility (TS always;
    /// Kotlin/Swift/Python/Rust since the Phase-3D readiness flip) override
    /// both to `Faithful` in the same breath — the hoisted declaration
    /// carries the named shape, including its recursion.
    fn classify_type_ref(&self, _name: &str) -> TypeMappingClass {
        self.classify_json()
    }
    /// Class of [`TargetTypeRenderer::array`]. `inner` is the classification
    /// the walker computed for the element type — a container carries its
    /// element's fidelity.
    fn classify_array(&self, inner_ty: &OirType, inner: TypeMappingClass) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::tuple`]: an array of the slot type when every slot
    /// agrees, else an array of `json`.
    fn classify_tuple(&self, items_ty: &[OirType], items: Vec<TypeMappingClass>) -> TypeMappingClass {
        match items_ty.first() {
            Some(first) if items_ty.iter().all(|t| t == first) => match items.into_iter().next() {
                Some(cls) => self.classify_array(first, cls),
                None => self.classify_array(&OirType::Json, self.classify_json()),
            },
            _ => self.classify_array(&OirType::Json, self.classify_json()),
        }
    }
    /// Class of [`TargetTypeRenderer::map`]. `key`/`value` are the children's
    /// classifications; a map is at best as faithful as its least faithful
    /// component (see [`TypeMappingClass::meet`]). Shells whose key rendering
    /// coerces or discards the key type (Rust collapses non-string-serializable
    /// keys) override this to reflect the coercion.
    fn classify_map(
        &self,
        key_ty: &OirType,
        key: TypeMappingClass,
        value_ty: &OirType,
        value: TypeMappingClass,
    ) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::optional`]. Carries the inner class, as
    /// `array` does.
    fn classify_optional(&self, inner_ty: &OirType, inner: TypeMappingClass) -> TypeMappingClass;
    /// Class of [`TargetTypeRenderer::struct_ty`].
    fn classify_struct_ty(&self, fields: &[OirField]) -> TypeMappingClass;
    /// Pair decision: the default renders via `json()`, so the default
    /// classifies as `json()`; shells with a real union facility — TS, Kotlin,
    /// Swift, Python, Rust (see [`TargetTypeRenderer::union_ty`]) — override
    /// both to `Faithful` in the same breath.
    fn classify_union_ty(&self, _variants: &[OirField]) -> TypeMappingClass {
        self.classify_json()
    }
}

/// Walk `ty`, dispatching each node to `r`. `enum_values` is the immediate
/// field's sidecar and reaches only the `String`/`Enum` leaves; every recursive
/// arm recurses with `None` (the sidecar applies at the field position, not to
/// nested element types).
pub fn render_type<R: TargetTypeRenderer + ?Sized>(
    r: &R,
    ty: &OirType,
    enum_values: Option<&[String]>,
) -> R::Out {
    match ty {
        OirType::String => r.string(enum_values),
        OirType::Number => r.number(),
        OirType::Integer => r.integer(),
        OirType::Boolean => r.boolean(),
        OirType::Date => r.date(),
        OirType::DateTime => r.datetime(),
        OirType::Email => r.email(),
        OirType::Url => r.url(),
        OirType::Phone => r.phone(),
        OirType::Uuid => r.uuid(),
        OirType::Image => r.image(),
        OirType::Bytes => r.bytes(),
        OirType::Money => r.money(),
        OirType::File => r.file(),
        OirType::Json => r.json(),
        OirType::Node => r.node(),
        OirType::Reference { entity } => r.reference(entity),
        OirType::Row { entity } => r.row(entity),
        OirType::Enum { name } => r.enum_ty(name, enum_values),
        OirType::Trait => r.trait_ref(),
        OirType::Orbital => r.orbital_value(),
        OirType::Behavior => r.behavior_value(),
        OirType::SExpr => r.sexpr(),
        OirType::TypeRef { name } => r.type_ref(name),
        OirType::Array { inner } => {
            let inner_out = render_type(r, inner, None);
            r.array(inner, inner_out)
        }
        OirType::Tuple { items } => {
            let outs = items.iter().map(|t| render_type(r, t, None)).collect();
            r.tuple(items, outs)
        }
        OirType::Map { key, value } => {
            let key_out = render_type(r, key, None);
            let value_out = render_type(r, value, None);
            r.map(key, key_out, value, value_out)
        }
        OirType::Optional { inner } => {
            let inner_out = render_type(r, inner, None);
            r.optional(inner, inner_out)
        }
        OirType::Struct { fields } => r.struct_ty(fields),
        OirType::Union { variants } => r.union_ty(variants),
    }
}

/// Classify `ty` under renderer `r`, dispatching the IDENTICAL `match` as
/// [`render_type`] — the two walkers must stay arm-for-arm the same so a
/// rendered type and its class can never be computed by different traversals.
/// `enum_values` has the same field-position-only reach as in [`render_type`].
pub fn classify_type<R: TargetTypeRenderer + ?Sized>(
    r: &R,
    ty: &OirType,
    enum_values: Option<&[String]>,
) -> TypeMappingClass {
    match ty {
        OirType::String => r.classify_string(enum_values),
        OirType::Number => r.classify_number(),
        OirType::Integer => r.classify_integer(),
        OirType::Boolean => r.classify_boolean(),
        OirType::Date => r.classify_date(),
        OirType::DateTime => r.classify_datetime(),
        OirType::Email => r.classify_email(),
        OirType::Url => r.classify_url(),
        OirType::Phone => r.classify_phone(),
        OirType::Uuid => r.classify_uuid(),
        OirType::Image => r.classify_image(),
        OirType::Bytes => r.classify_bytes(),
        OirType::Money => r.classify_money(),
        OirType::File => r.classify_file(),
        OirType::Json => r.classify_json(),
        OirType::Node => r.classify_node(),
        OirType::Reference { entity } => r.classify_reference(entity),
        OirType::Row { entity } => r.classify_row(entity),
        OirType::Enum { name } => r.classify_enum_ty(name, enum_values),
        OirType::Trait => r.classify_trait_ref(),
        OirType::Orbital => r.classify_orbital_value(),
        OirType::Behavior => r.classify_behavior_value(),
        OirType::SExpr => r.classify_sexpr(),
        OirType::TypeRef { name } => r.classify_type_ref(name),
        OirType::Array { inner } => {
            let inner_cls = classify_type(r, inner, None);
            r.classify_array(inner, inner_cls)
        }
        OirType::Tuple { items } => {
            let classes = items.iter().map(|t| classify_type(r, t, None)).collect();
            r.classify_tuple(items, classes)
        }
        OirType::Map { key, value } => {
            let key_cls = classify_type(r, key, None);
            let value_cls = classify_type(r, value, None);
            r.classify_map(key, key_cls, value, value_cls)
        }
        OirType::Optional { inner } => {
            let inner_cls = classify_type(r, inner, None);
            r.classify_optional(inner, inner_cls)
        }
        OirType::Struct { fields } => r.classify_struct_ty(fields),
        OirType::Union { variants } => r.classify_union_ty(variants),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `String`-valued renderer that tags every node with its kind. It lets
    /// the characterization tests pin exactly which trait method the walker
    /// dispatches each `OirType` variant to, and prove that recursion drops the
    /// field-level `enum_values` sidecar.
    struct TagRenderer;

    impl TargetTypeRenderer for TagRenderer {
        type Out = String;

        fn string(&self, enum_values: Option<&[String]>) -> String {
            match enum_values {
                Some(v) => format!("string<{}>", v.join("|")),
                None => "string".to_string(),
            }
        }
        fn number(&self) -> String {
            "number".to_string()
        }
        fn integer(&self) -> String {
            "integer".to_string()
        }
        fn boolean(&self) -> String {
            "boolean".to_string()
        }
        fn date(&self) -> String {
            "date".to_string()
        }
        fn json(&self) -> String {
            "json".to_string()
        }
        fn reference(&self, entity: &str) -> String {
            format!("ref({entity})")
        }
        fn row(&self, entity: &str) -> String {
            format!("row({entity})")
        }
        fn enum_ty(&self, name: &str, enum_values: Option<&[String]>) -> String {
            match enum_values {
                Some(v) => format!("enum({name})<{}>", v.join("|")),
                None => format!("enum({name})"),
            }
        }
        fn trait_ref(&self) -> String {
            "trait".to_string()
        }
        fn sexpr(&self) -> String {
            "sexpr".to_string()
        }
        fn array(&self, _inner_ty: &OirType, inner: String) -> String {
            format!("array<{inner}>")
        }
        fn map(&self, _k: &OirType, key: String, _v: &OirType, value: String) -> String {
            format!("map<{key},{value}>")
        }
        fn optional(&self, _inner_ty: &OirType, inner: String) -> String {
            format!("optional<{inner}>")
        }
        fn struct_ty(&self, fields: &[OirField]) -> String {
            let inner: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}:{}",
                        f.name,
                        render_type(self, &f.field_type, f.enum_values.as_deref())
                    )
                })
                .collect();
            format!("struct<{}>", inner.join(","))
        }

        fn classify_string(&self, _enum_values: Option<&[String]>) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_number(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_integer(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_boolean(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_date(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_json(&self) -> TypeMappingClass {
            TypeMappingClass::Lossy
        }
        fn classify_reference(&self, _entity: &str) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_row(&self, _entity: &str) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_enum_ty(
            &self,
            _name: &str,
            _enum_values: Option<&[String]>,
        ) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_trait_ref(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_sexpr(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_array(&self, _inner_ty: &OirType, inner: TypeMappingClass) -> TypeMappingClass {
            inner
        }
        fn classify_map(
            &self,
            _key_ty: &OirType,
            key: TypeMappingClass,
            _value_ty: &OirType,
            value: TypeMappingClass,
        ) -> TypeMappingClass {
            key.meet(value)
        }
        fn classify_optional(
            &self,
            _inner_ty: &OirType,
            inner: TypeMappingClass,
        ) -> TypeMappingClass {
            inner
        }
        fn classify_struct_ty(&self, _fields: &[OirField]) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
    }

    fn tag(ty: &OirType) -> String {
        render_type(&TagRenderer, ty, None)
    }

    /// Implements only the REQUIRED methods — proves `reference` and
    /// `classify_reference` compile and behave correctly as provided defaults.
    struct MinimalRenderer;

    impl TargetTypeRenderer for MinimalRenderer {
        type Out = String;

        fn string(&self, enum_values: Option<&[String]>) -> String {
            match enum_values {
                Some(v) => format!("string<{}>", v.join("|")),
                None => "string".to_string(),
            }
        }
        fn number(&self) -> String {
            "number".to_string()
        }
        fn integer(&self) -> String {
            "integer".to_string()
        }
        fn boolean(&self) -> String {
            "boolean".to_string()
        }
        fn date(&self) -> String {
            "date".to_string()
        }
        fn json(&self) -> String {
            "json".to_string()
        }
        fn row(&self, entity: &str) -> String {
            format!("row({entity})")
        }
        fn enum_ty(&self, name: &str, _enum_values: Option<&[String]>) -> String {
            format!("enum({name})")
        }
        fn trait_ref(&self) -> String {
            "trait".to_string()
        }
        fn sexpr(&self) -> String {
            "sexpr".to_string()
        }
        fn array(&self, _inner_ty: &OirType, inner: String) -> String {
            format!("array<{inner}>")
        }
        fn map(&self, _k: &OirType, key: String, _v: &OirType, value: String) -> String {
            format!("map<{key},{value}>")
        }
        fn optional(&self, _inner_ty: &OirType, inner: String) -> String {
            format!("optional<{inner}>")
        }
        fn struct_ty(&self, _fields: &[OirField]) -> String {
            "struct".to_string()
        }

        fn classify_string(&self, _enum_values: Option<&[String]>) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_number(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_integer(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_boolean(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_date(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_row(&self, _entity: &str) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_json(&self) -> TypeMappingClass {
            TypeMappingClass::Lossy
        }
        fn classify_enum_ty(
            &self,
            _name: &str,
            _enum_values: Option<&[String]>,
        ) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_trait_ref(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_sexpr(&self) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
        fn classify_array(&self, _inner_ty: &OirType, inner: TypeMappingClass) -> TypeMappingClass {
            inner
        }
        fn classify_map(
            &self,
            _key_ty: &OirType,
            key: TypeMappingClass,
            _value_ty: &OirType,
            value: TypeMappingClass,
        ) -> TypeMappingClass {
            key.meet(value)
        }
        fn classify_optional(
            &self,
            _inner_ty: &OirType,
            inner: TypeMappingClass,
        ) -> TypeMappingClass {
            inner
        }
        fn classify_struct_ty(&self, _fields: &[OirField]) -> TypeMappingClass {
            TypeMappingClass::Faithful
        }
    }

    #[test]
    fn reference_defaults_to_the_string_leaf() {
        let r = MinimalRenderer;
        assert_eq!(
            render_type(&r, &OirType::reference("Order"), None),
            r.string(None)
        );
        assert_eq!(
            classify_type(&r, &OirType::reference("Order"), None),
            TypeMappingClass::Widening
        );
    }

    #[test]
    fn row_dispatches_to_row_not_reference() {
        let r = MinimalRenderer;
        assert_eq!(render_type(&r, &OirType::row("Order"), None), "row(Order)");
        assert_ne!(
            render_type(&r, &OirType::row("Order"), None),
            render_type(&r, &OirType::reference("Order"), None)
        );
        assert_eq!(
            classify_type(&r, &OirType::row("Order"), None),
            TypeMappingClass::Faithful
        );
        assert_eq!(
            classify_type(&r, &OirType::reference("Order"), None),
            TypeMappingClass::Widening
        );
        assert_eq!(
            render_type(&r, &OirType::array(OirType::row("Order")), None),
            "array<row(Order)>"
        );
    }

    #[test]
    fn every_leaf_dispatches_to_its_method() {
        assert_eq!(tag(&OirType::String), "string");
        assert_eq!(tag(&OirType::Number), "number");
        assert_eq!(tag(&OirType::Integer), "integer");
        assert_eq!(tag(&OirType::Boolean), "boolean");
        assert_eq!(tag(&OirType::Date), "date");
        // DateTime defaults to Date via the provided method.
        assert_eq!(tag(&OirType::DateTime), "date");
        assert_eq!(tag(&OirType::Json), "json");
        assert_eq!(tag(&OirType::reference("User")), "ref(User)");
        assert_eq!(tag(&OirType::enum_type("Status")), "enum(Status)");
        assert_eq!(tag(&OirType::Trait), "trait");
        assert_eq!(tag(&OirType::SExpr), "sexpr");
        // Node defaults to the target's json rendering via the provided method.
        assert_eq!(tag(&OirType::Node), "json");
    }

    #[test]
    fn recursive_arms_compose_children() {
        assert_eq!(tag(&OirType::array(OirType::Number)), "array<number>");
        assert_eq!(
            tag(&OirType::map(OirType::String, OirType::Boolean)),
            "map<string,boolean>"
        );
        // `OirType::optional` is idempotent (C2), so a literally-nested
        // `Optional<Optional<_>>` — still a legal IR shape off the wire —
        // is built with a struct literal to keep this walker-recursion case.
        let nested_optional = OirType::Optional {
            inner: Box::new(OirType::Optional {
                inner: Box::new(OirType::Integer),
            }),
        };
        assert_eq!(tag(&nested_optional), "optional<optional<integer>>");
        assert_eq!(
            tag(&OirType::array(OirType::reference("Order"))),
            "array<ref(Order)>"
        );
    }

    #[test]
    fn field_enum_values_reach_string_and_enum_leaves_only() {
        let vals = vec!["a".to_string(), "b".to_string()];
        // At the immediate field position the sidecar reaches the String leaf.
        assert_eq!(
            render_type(&TagRenderer, &OirType::String, Some(&vals)),
            "string<a|b>"
        );
        assert_eq!(
            render_type(&TagRenderer, &OirType::enum_type("Kind"), Some(&vals)),
            "enum(Kind)<a|b>"
        );
        // Recursion forces the sidecar to None, so a nested String is bare.
        assert_eq!(
            render_type(&TagRenderer, &OirType::array(OirType::String), Some(&vals)),
            "array<string>"
        );
    }

    #[test]
    fn struct_backend_owns_per_field_sidecar() {
        let ty = OirType::Struct {
            fields: vec![
                OirField {
                    declared: None,
                    name: "kind".to_string(),
                    field_type: OirType::String,
                    description: None,
                    required: true,
                    default: None,
                    constraints: vec![],
                    enum_values: Some(vec!["x".to_string(), "y".to_string()]),
                    relation: None,
                    intrinsic: false,
                    merged_from: None,
                },
                OirField::new("count", OirType::Integer),
            ],
        };
        assert_eq!(tag(&ty), "struct<kind:string<x|y>,count:integer>");
    }
}
