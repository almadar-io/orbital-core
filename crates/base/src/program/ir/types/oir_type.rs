//! `OirType`, the type of a field, payload or value in the IR.

use super::*;

/// Universal type system
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum OirType {
    String,
    Number,
    Integer,
    Boolean,
    Date,
    DateTime,
    /// Epoch milliseconds the clock produced (`time/now`, `time/today`, and time arithmetic over an instant): always a
    /// valid moment, so formatting it cannot fail. Nothing widens into it; it fits wherever a number does.
    Instant,
    /// Semantic string domains. Every target renders these as its string type;
    /// they exist in the IR so `oir_type_to_mock_type` / `oir_type_to_form_type`
    /// can dispatch on the declared domain instead of guessing from a name.
    Email,
    Url,
    Phone,
    Uuid,
    Image,
    /// Monetary amount — numeric at rest. Distinct from `Number` so codegen
    /// and renderers dispatch currency formatting/inputs on the declared
    /// type instead of guessing from a field name.
    Money,
    /// Uploaded file/attachment — a STRUCT value at rest:
    /// `{ name: string, url: string, mimeType: string, sizeBytes: number }`.
    /// `url` is a `data:` URI in mock mode or an object-storage URL in
    /// production. Not a semantic string: it names a stored artifact.
    File,
    /// Binary value; standard padded base64 string at run time, never
    /// implicitly a `string`.
    Bytes,
    /// A call key: a string at rest, but a bare `string` converts to it only through
    /// the explicit cast (`Coercion::Explicit`). Distinct from the semantic string
    /// domains, whose `String` source is refined implicitly.
    CallKey,
    /// The name of a field of the record `record`, a string at run time; `value` is the type that field must have,
    /// when declared. Read through `object/get` of a record that is an `R`, it types the read.
    #[serde(rename_all = "camelCase")]
    FieldRef {
        record: Box<OirType>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Box<OirType>>,
    },
    Json,
    /// Renderable UI content (a `node`-typed field/knob): a pattern node or
    /// array of them. Distinct from `Json` so codegen can emit the canonical
    /// pattern-tree type instead of a shapeless record.
    Node,
    #[serde(rename_all = "camelCase")]
    Array {
        inner: Box<OirType>,
    },
    /// Fixed-length array with a type per slot (`.lolo` `[number, number, number]`).
    #[serde(rename_all = "camelCase")]
    Tuple {
        items: Vec<OirType>,
    },
    /// Dynamic-key uniform-value map (`.lolo` `Map K V`) — emits
    /// `Record<K, V>` in codegen. Keys are arbitrary (not known at author
    /// time); the value type is uniform. Distinct from `Struct` (fixed keys).
    #[serde(rename_all = "camelCase")]
    Map {
        key: Box<OirType>,
        value: Box<OirType>,
    },
    #[serde(rename_all = "camelCase")]
    Optional {
        inner: Box<OirType>,
    },
    #[serde(rename_all = "camelCase")]
    Reference {
        entity: String,
    },
    /// An embedded full row of `entity` (an `@intrinsic` list-of-entity
    /// field), distinct from `Reference`, which is the id/relation value.
    #[serde(rename_all = "camelCase")]
    Row {
        entity: String,
    },
    #[serde(rename_all = "camelCase")]
    Enum {
        name: String,
    },
    /// Inline struct (anonymous object) — preserves the field shape
    /// declared by a lolo struct type alias (`type X = { f1: T1, f2: T2 }`)
    /// so codegen can emit a strict TypeScript type instead of widening
    /// to `Record<string, FieldValue | undefined>`.
    Struct {
        fields: Shared<Vec<OirField>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional: Option<Box<OirType>>,
    },
    /// A TAGGED union of struct variants. `variants` are in declaration order;
    /// each carries its own name so codegen can emit a discriminated union and
    /// the validator can select a variant by the value's tag field. A variant
    /// whose shape is a by-name recursive back-reference resolves to
    /// `OirType::TypeRef` rather than expanding forever.
    Union {
        variants: Shared<Vec<OirField>>,
    },
    /// `@trait.X` reference type. Allowed only on trait config fields. The
    /// codegen + runtime substitute string values matching `@trait.<Name>`
    /// with embedded trait UI at render time. Maps to the `TraitFieldRef`
    /// branded string type in the emitted TypeScript.
    Trait,
    /// An orbital value held as data (language trio): `{ behavior, orbital, …§8b body }`
    /// or `none`. Never rendered — only a trait value embeds.
    Orbital,
    /// A whole behavior held as data: `{ behavior }` or `none`; embeds as a running program.
    Behavior,
    /// Compiled s-expression (`SExpr` from `@almadar/core`). Allowed only on
    /// trait config fields (e.g. a `conditionalFields` map's value). Emits the
    /// recursive `SExpr` TypeScript type, inlined as a local alias in codegen
    /// (like `TraitFieldRef`) so no cross-file import is needed.
    SExpr,
    /// A by-name reference to a named struct type declared elsewhere in the
    /// same expansion tree — same wire shape as the struct it names. Exists
    /// so a self-referential `type X = { … : [X] }` can be represented at
    /// all: a recursive back-reference cannot be INLINED (the `.orb` JSON is
    /// finite), so the resolver stops expanding at the cycle and leaves a
    /// `TypeRef` naming what it would have expanded to. Codegen backends with
    /// a named-type facility (the TS shell) hoist one declaration per name and
    /// emit a real recursive reference; every other backend renders the wire shape (see
    /// `ir/render.rs`'s default arm).
    TypeRef {
        name: String,
    },
    /// An unbound type variable (`t` of `type Page t` or a trait's `(t : Type)`):
    /// opaque, so it accepts only itself, or `Json` by erasure.
    Var {
        name: String,
    },
}

/// The empty field list every fieldless record and empty union shares: building one allocates nothing.
pub fn no_fields() -> Shared<Vec<OirField>> {
    static EMPTY: crate::sync::OnceLock<Shared<Vec<OirField>>> = crate::sync::OnceLock::new();
    EMPTY.get_or_init(Default::default).clone()
}

impl OirType {
    /// Rewrite existing type nodes; replacements are terminal for this traversal.
    pub fn rewrite(&mut self, replace: &impl Fn(&OirType) -> Option<OirType>) -> bool {
        if let Some(next) = replace(self) {
            let changed = *self != next;
            *self = next;
            return changed;
        }
        match self {
            Self::Array { inner } | Self::Optional { inner } => inner.rewrite(replace),
            Self::Map { key, value } => {
                let key_changed = key.rewrite(replace);
                value.rewrite(replace) | key_changed
            }
            Self::Tuple { items } => {
                let mut changed = false;
                for item in items {
                    changed |= item.rewrite(replace);
                }
                changed
            }
            Self::Struct { fields, additional } => {
                let mut changed = false;
                for field in fields { changed |= field.field_type.rewrite(replace); }
                if let Some(additional) = additional { changed |= additional.rewrite(replace); }
                changed
            }
            Self::FieldRef { record, value } => {
                let record_changed = record.rewrite(replace);
                match value {
                    Some(value) => value.rewrite(replace) | record_changed,
                    None => record_changed,
                }
            }
            Self::Union { variants: fields } => {
                let mut changed = false;
                for field in fields {
                    changed |= field.field_type.rewrite(replace);
                }
                changed
            }
            _ => false,
        }
    }

    pub fn substitute_type_vars(
        &mut self,
        bindings: &std::collections::BTreeMap<String, OirType>,
    ) -> bool {
        self.rewrite(&|ty| match ty {
            Self::Var { name } => bindings.get(name).cloned(),
            _ => None,
        })
    }

    /// Create an array type
    pub fn array(inner: OirType) -> Self {
        Self::Array {
            inner: Box::new(inner),
        }
    }

    /// Create a dynamic-key map type (`Record<key, value>`).
    pub fn map(key: OirType, value: OirType) -> Self {
        Self::Map {
            key: Box::new(key),
            value: Box::new(value),
        }
    }

    /// Create an optional type. Idempotent: wrapping an already-`Optional`
    /// type returns it unchanged rather than double-wrapping.
    /// The type at an optional-aware position: unwraps exactly one `Optional` layer and
    /// reports whether the position is optional (wrapped, or declared `required:false`).
    pub fn effective(&self, required: bool) -> (&OirType, bool) {
        match self {
            OirType::Optional { inner } => (inner.as_ref(), true),
            t => (t, !required),
        }
    }

    pub fn optional(inner: OirType) -> Self {
        if matches!(inner, Self::Optional { .. }) {
            return inner;
        }
        Self::Optional {
            inner: Box::new(inner),
        }
    }

    /// Create a reference type
    pub fn reference(entity: impl Into<String>) -> Self {
        Self::Reference {
            entity: entity.into(),
        }
    }

    /// Create an embedded-row type
    pub fn row(entity: impl Into<String>) -> Self {
        Self::Row {
            entity: entity.into(),
        }
    }

    /// Create an enum type
    pub fn enum_type(name: impl Into<String>) -> Self {
        Self::Enum { name: name.into() }
    }

    /// Check if this type is optional
    pub fn is_optional(&self) -> bool {
        matches!(self, Self::Optional { .. })
    }

    /// Get the inner type if this is an array or optional
    pub fn inner_type(&self) -> Option<&OirType> {
        match self {
            Self::Array { inner } | Self::Optional { inner } => Some(inner),
            _ => None,
        }
    }
}
