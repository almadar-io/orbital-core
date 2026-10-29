//! What the Rust wire types' serde derives accept, read from their source: the generated TypeScript decoders take
//! exactly this (integer ranges, unknown fields, defaults), never a list of names.

use std::collections::BTreeMap;
use std::path::Path;

use syn::{Attribute, Fields, Item, Type};

#[derive(Debug, Clone)]
pub enum DefaultKind {
    No,
    Trait,
    Path(String),
}

#[derive(Clone)]
pub struct FieldInfo {
    pub key: String,
    pub ty: Type,
    pub default: DefaultKind,
    pub skippable: bool,
}

#[derive(Clone)]
pub struct VariantInfo {
    pub tag: String,
    pub fields: Vec<FieldInfo>,
}

#[derive(Clone)]
pub enum TypeInfo {
    Struct { fields: Vec<FieldInfo>, deny: bool, derives_default: bool },
    Enum { tag: Option<String>, variants: Vec<VariantInfo>, deny: bool },
    Custom(String),
}

/// serde's `rename_all` rules applied to a Rust identifier: a snake_case field or a PascalCase variant.
fn renamed(ident: &str, rule: &str, variant: bool) -> String {
    let snake = |ident: &str| -> String {
        if !variant {
            return ident.to_string();
        }
        let mut out = String::new();
        for (i, c) in ident.chars().enumerate() {
            if c.is_uppercase() && i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        }
        out
    };
    let pascal = |ident: &str| -> String {
        if variant {
            return ident.to_string();
        }
        let mut out = String::new();
        let mut upper = true;
        for c in ident.chars() {
            if c == '_' {
                upper = true;
            } else if upper {
                out.extend(c.to_uppercase());
                upper = false;
            } else {
                out.push(c);
            }
        }
        out
    };
    match rule {
        "lowercase" => ident.to_lowercase(),
        "UPPERCASE" => ident.to_uppercase(),
        "PascalCase" => pascal(ident),
        "camelCase" => {
            let p = pascal(ident);
            let mut chars = p.chars();
            chars.next().map(|c| c.to_lowercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        }
        "snake_case" => snake(ident),
        "SCREAMING_SNAKE_CASE" => snake(ident).to_uppercase(),
        "kebab-case" => snake(ident).replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => snake(ident).to_uppercase().replace('_', "-"),
        other => panic!("serde_index: unknown rename_all rule `{other}`"),
    }
}

#[derive(Default)]
struct Attrs {
    rename: Option<String>,
    rename_all: Option<String>,
    rename_all_fields: Option<String>,
    deny: bool,
    tag: Option<String>,
    content: bool,
    untagged: bool,
    default: Option<DefaultKind>,
    skip: bool,
    skippable: bool,
    flatten: bool,
    custom: Option<String>,
    alias: bool,
}

fn serde_attrs(attrs: &[Attribute]) -> Attrs {
    let mut out = Attrs::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            let key = meta.path.get_ident().map(|i| i.to_string()).unwrap_or_default();
            let value = if meta.input.peek(syn::Token![=]) {
                Some(meta.value()?.parse::<syn::LitStr>()?.value())
            } else if meta.input.peek(syn::token::Paren) {
                meta.parse_nested_meta(|_| Ok(()))?;
                out.custom = Some(format!("{key}(..)"));
                return Ok(());
            } else {
                None
            };
            match (key.as_str(), value) {
                ("rename", Some(v)) => out.rename = Some(v),
                ("rename_all", Some(v)) => out.rename_all = Some(v),
                ("rename_all_fields", Some(v)) => out.rename_all_fields = Some(v),
                ("deny_unknown_fields", None) => out.deny = true,
                ("tag", Some(v)) => out.tag = Some(v),
                ("content", Some(_)) => out.content = true,
                ("untagged", None) => out.untagged = true,
                ("default", None) => out.default = Some(DefaultKind::Trait),
                ("default", Some(path)) => out.default = Some(DefaultKind::Path(path)),
                ("skip" | "skip_deserializing", None) => out.skip = true,
                ("skip_serializing_if", _) => out.skippable = true,
                ("skip_serializing" | "serialize_with" | "bound", _) => {}
                ("flatten", None) => out.flatten = true,
                ("transparent", None) => out.custom = Some("transparent".to_string()),
                ("alias", _) => out.alias = true,
                ("with" | "deserialize_with" | "from" | "try_from" | "into", v) => out.custom = Some(format!("{key} = {v:?}")),
                (other, _) => out.custom = Some(format!("serde attribute `{other}`")),
            }
            Ok(())
        })
        .unwrap_or_else(|e| {
            out.custom = Some(format!("an unreadable serde attribute: {e}"));
        });
    }
    out
}

fn derives_serde(attrs: &[Attribute]) -> bool {
    derives(attrs, "Deserialize") || derives(attrs, "Serialize")
}

/// The tokens of a type, for telling two definitions of one name apart.
fn signature(info: &TypeInfo) -> String {
    use quote::ToTokens;
    let fields = |fields: &[FieldInfo]| {
        fields.iter().map(|f| format!("{}:{}:{}", f.key, f.ty.to_token_stream(), f.skippable)).collect::<Vec<_>>().join(",")
    };
    match info {
        TypeInfo::Struct { fields: f, deny, .. } => format!("struct {deny} {}", fields(f)),
        TypeInfo::Enum { tag, variants, deny } => format!(
            "enum {tag:?} {deny} {}",
            variants.iter().map(|v| format!("{}({})", v.tag, fields(&v.fields))).collect::<Vec<_>>().join(";")
        ),
        TypeInfo::Custom(why) => format!("custom {why}"),
    }
}

fn derives(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().filter(|a| a.path().is_ident("derive")).any(|a| {
        let mut found = false;
        let _ = a.parse_nested_meta(|m| {
            if m.path.segments.last().is_some_and(|s| s.ident == name) {
                found = true;
            }
            Ok(())
        });
        found
    })
}

fn fields_of(fields: &Fields, rule: Option<&str>, container_default: bool) -> Result<Vec<FieldInfo>, String> {
    let Fields::Named(named) = fields else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for field in &named.named {
        let attrs = serde_attrs(&field.attrs);
        if attrs.flatten {
            return Err("a flattened field".to_string());
        }
        if attrs.alias {
            return Err("a field alias".to_string());
        }
        if let Some(why) = attrs.custom {
            return Err(why);
        }
        if attrs.skip {
            continue;
        }
        let ident = field.ident.as_ref().expect("named").to_string().trim_start_matches("r#").to_string();
        let key = attrs.rename.or_else(|| rule.map(|r| renamed(&ident, r, false))).unwrap_or(ident);
        let default = attrs.default.unwrap_or(if container_default { DefaultKind::Trait } else { DefaultKind::No });
        out.push(FieldInfo { key, ty: field.ty.clone(), default, skippable: attrs.skippable });
    }
    Ok(out)
}

fn collect(items: &[Item], out: &mut BTreeMap<String, Vec<TypeInfo>>) {
    for item in items {
        match item {
            Item::Struct(s) if derives_serde(&s.attrs) => {
                let attrs = serde_attrs(&s.attrs);
                let name = s.ident.to_string();
                let info = match attrs.custom {
                    Some(why) => TypeInfo::Custom(why),
                    None => match &s.fields {
                        Fields::Named(_) => match fields_of(&s.fields, attrs.rename_all.as_deref(), attrs.default.is_some()) {
                            Ok(fields) => TypeInfo::Struct { fields, deny: attrs.deny, derives_default: derives(&s.attrs, "Default") },
                            Err(why) => TypeInfo::Custom(why),
                        },
                        _ => TypeInfo::Custom("a tuple or unit struct".to_string()),
                    },
                };
                out.entry(name).or_default().push(info);
            }
            Item::Enum(e) if derives_serde(&e.attrs) => {
                let attrs = serde_attrs(&e.attrs);
                let name = e.ident.to_string();
                let info = if let Some(why) = attrs.custom {
                    TypeInfo::Custom(why)
                } else if attrs.untagged || attrs.content {
                    TypeInfo::Custom("an untagged or adjacently tagged enum".to_string())
                } else {
                    let mut failure = None;
                    let variants: Vec<VariantInfo> = e
                        .variants
                        .iter()
                        .filter_map(|v| {
                            let va = serde_attrs(&v.attrs);
                            if va.skip {
                                return None;
                            }
                            if let Some(why) = va.custom {
                                failure = Some(why);
                                return None;
                            }
                            let ident = v.ident.to_string();
                            let tag = va
                                .rename
                                .clone()
                                .or_else(|| attrs.rename_all.as_deref().map(|r| renamed(&ident, r, true)))
                                .unwrap_or(ident);
                            let rule = va.rename_all.as_deref().or(attrs.rename_all_fields.as_deref());
                            match fields_of(&v.fields, rule, va.default.is_some()) {
                                Ok(fields) => Some(VariantInfo { tag, fields }),
                                Err(why) => {
                                    failure = Some(why);
                                    None
                                }
                            }
                        })
                        .collect();
                    match failure {
                        Some(why) => TypeInfo::Custom(why),
                        None => TypeInfo::Enum { tag: attrs.tag, variants, deny: attrs.deny },
                    }
                };
                out.entry(name).or_default().push(info);
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, out);
                }
            }
            _ => {}
        }
    }
}

fn walk(dir: &Path, out: &mut BTreeMap<String, Vec<TypeInfo>>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let source = std::fs::read_to_string(&path).expect("a readable source file");
            let Ok(file) = syn::parse_file(&source) else { continue };
            collect(&file.items, out);
        }
    }
}

pub fn index_of_source(source: &str) -> BTreeMap<String, Vec<TypeInfo>> {
    let file = syn::parse_file(source).unwrap_or_else(|e| panic!("serde_index: the source does not parse: {e}"));
    let mut out = BTreeMap::new();
    collect(&file.items, &mut out);
    out
}

/// Every `Deserialize`- or `Serialize`-deriving struct and enum under the orbital-core crates' `src`, by Rust name; the
/// definitions of one name that read the same collapse to one.
pub fn index() -> BTreeMap<String, Vec<TypeInfo>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut out = BTreeMap::new();
    for krate in std::fs::read_dir(&root).expect("the orbital-core crates directory").flatten() {
        walk(&krate.path().join("src"), &mut out);
    }
    for infos in out.values_mut() {
        let mut seen = Vec::new();
        infos.retain(|info| {
            let sig = signature(info);
            let fresh = !seen.contains(&sig);
            seen.push(sig);
            fresh
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_rules_follow_serde() {
        assert_eq!(renamed("entity_type", "camelCase", false), "entityType");
        assert_eq!(renamed("ReadRow", "camelCase", true), "readRow");
        assert_eq!(renamed("PageFacts", "camelCase", true), "pageFacts");
        assert_eq!(renamed("PageFacts", "snake_case", true), "page_facts");
        assert_eq!(renamed("ttl_ms", "kebab-case", false), "ttl-ms");
        assert_eq!(renamed("Google", "lowercase", true), "google");
        assert_eq!(renamed("entity_type", "PascalCase", false), "EntityType");
        assert_eq!(renamed("entity_type", "SCREAMING_SNAKE_CASE", false), "ENTITY_TYPE");
    }

    #[test]
    fn the_index_reads_serde_attributes_of_a_wire_type() {
        let all = index();
        let Some(TypeInfo::Struct { fields, deny, .. }) = all["PageFactsWire"].first() else { panic!("a struct") };
        assert!(!deny);
        assert!(fields.iter().any(|f| f.key == "screenWidth"));
        let Some(TypeInfo::Struct { deny, .. }) = all["OsGrants"].first() else { panic!("a struct") };
        assert!(*deny, "OsGrants denies unknown fields");
    }
}
