//! The embedded service registry: actions, params and credentials.

use super::*;

/// The raw services-registry.json text, from the installed `@almadar/core`.
pub fn services_json() -> &'static str {
    crate::language_data::language_data_after_preflight()
        .services
        .as_str()
}

/// Service name → declared action names, from the baked services registry.
/// A declared-only service (host-injected handler, no static contract) is
/// present with an EMPTY set — its name is known, its actions are unchecked.
///
/// Read from the installed `@almadar/core` (`crate::language_data`).
pub fn baked_service_actions() -> &'static HashMap<String, HashSet<String>> {
    &baked_service_registry().actions
}

/// One declared parameter of a service action, from the services registry.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedServiceParam {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    /// An array param's element shape: its members' declared types.
    #[serde(default)]
    pub items: Option<EmbeddedParamItems>,
    /// A struct param's members.
    #[serde(default)]
    pub properties: Option<HashMap<String, EmbeddedParamMember>>,
    /// A dictionary param's value type.
    #[serde(default, rename = "mapValue")]
    pub map_value: Option<EmbeddedParamMember>,
}

/// The element shape of an array service param.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbeddedParamItems {
    #[serde(default)]
    pub properties: HashMap<String, EmbeddedParamMember>,
}

/// One member of an array param's element.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbeddedParamMember {
    #[serde(default, rename = "type")]
    pub member_type: String,
}

/// The `@almadar/core` `EventAddress` contract type: a `listens`-addressed
/// event value, checked against the program's declared external inputs.
pub const EVENT_ADDRESS_TYPE: &str = "EventAddress";

/// The validation projections of the services registry: declared action names
/// plus per-action param contracts. One parse, one source — the CLI's live
/// CWD-registry load and this baked fallback both produce it via
/// [`parse_service_registry_data`], so the action gate and the param checks
/// (I-13) can never draw from two different registry generations.
#[derive(Debug, Clone, Default)]
pub struct ServiceRegistryData {
    /// Service name → declared action names. A declared-only service
    /// (host-injected handler, no static contract) carries an EMPTY set —
    /// its name is known, its actions are unchecked.
    pub actions: HashMap<String, HashSet<String>>,
    /// Service name → action name → declared params.
    pub params: HashMap<String, HashMap<String, Vec<EmbeddedServiceParam>>>,
    /// Service name → the runtimes its implementation declares (`node`, `browser`).
    /// A service without the field declares none.
    pub runtimes: HashMap<String, Vec<String>>,
    /// Service name → action name → the fields of the action's `responseShape`
    /// record, sorted by name. An action with no (or an empty) `responseShape`
    /// is absent: its result is not a known record.
    pub response_shapes: HashMap<String, HashMap<String, Vec<ResponseShapeField>>>,
}

/// One field of a service action's `responseShape` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseShapeField {
    pub name: String,
    pub field_type: String,
}

impl ServiceRegistryData {
    /// The fields of `service.action`'s `responseShape`, or `None` when the
    /// action declares none.
    pub fn response_shape(&self, service: &str, action: &str) -> Option<&[ResponseShapeField]> {
        self.response_shapes
            .get(service)?
            .get(action)
            .map(Vec::as_slice)
    }
}

/// The registry is written from TS contracts: its list type `T[]` is the `.lolo` `[T]`.
/// A TS string-literal union `'a' | 'b'` is the `.lolo` `"a" | "b"`.
pub fn lolo_spelling(t: &str) -> String {
    match t.strip_suffix("[]").filter(|base| !base.is_empty()) {
        Some(base) => {
            let element = base
                .strip_prefix('(')
                .and_then(|b| b.strip_suffix(')'))
                .unwrap_or(base);
            format!("[{}]", lolo_spelling(element))
        }
        None => literal_union_spelling(t).unwrap_or_else(|| t.to_string()),
    }
}

pub(super) fn literal_union_spelling(t: &str) -> Option<String> {
    t.split(" | ")
        .map(|arm| {
            arm.strip_prefix('\'')
                .and_then(|a| a.strip_suffix('\''))
                .filter(|inner| !inner.contains(['\'', '"', '\\']))
                .map(|inner| format!("\"{inner}\""))
        })
        .collect::<Option<Vec<_>>>()
        .map(|arms| arms.join(" | "))
}

pub fn response_shape_fields(
    shape: &serde_json::Map<String, serde_json::Value>,
) -> Vec<ResponseShapeField> {
    let mut fields: Vec<ResponseShapeField> = shape
        .iter()
        .map(|(name, value)| ResponseShapeField {
            name: name.clone(),
            field_type: match value {
                serde_json::Value::String(t) => lolo_spelling(t),
                serde_json::Value::Object(o) => o
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| "any".to_string(), lolo_spelling),
                _ => "any".to_string(),
            },
        })
        .collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

/// Parse raw services-registry.json text into both validation projections.
pub fn parse_service_registry_data(raw: &str) -> Result<ServiceRegistryData, serde_json::Error> {
    #[derive(Deserialize)]
    struct ActionEntry {
        name: String,
        #[serde(default)]
        params: Vec<EmbeddedServiceParam>,
        #[serde(default, rename = "responseShape")]
        response_shape: Option<serde_json::Map<String, serde_json::Value>>,
    }
    #[derive(Deserialize)]
    struct ServiceEntry {
        #[serde(default)]
        actions: Vec<ActionEntry>,
        #[serde(default)]
        runtimes: Vec<String>,
    }
    #[derive(Deserialize)]
    struct ServicesRegistry {
        #[serde(default)]
        services: HashMap<String, ServiceEntry>,
    }
    let registry: ServicesRegistry = serde_json::from_str(raw)?;
    let mut data = ServiceRegistryData::default();
    for (name, entry) in registry.services {
        let mut actions = HashSet::new();
        let mut params = HashMap::new();
        let mut shapes = HashMap::new();
        for a in entry.actions {
            actions.insert(a.name.clone());
            if let Some(shape) = a.response_shape.filter(|s| !s.is_empty()) {
                shapes.insert(a.name.clone(), response_shape_fields(&shape));
            }
            params.insert(a.name, a.params);
        }
        data.response_shapes.insert(name.clone(), shapes);
        data.actions.insert(name.clone(), actions);
        data.params.insert(name.clone(), params);
        data.runtimes.insert(name, entry.runtimes);
    }
    Ok(data)
}

/// The installed services registry, parsed once.
pub fn baked_service_registry() -> &'static ServiceRegistryData {
    static DATA: OnceLock<ServiceRegistryData> = OnceLock::new();
    DATA.get_or_init(|| {
        let raw = services_json();
        parse_service_registry_data(raw)
            .expect("installed services-registry.json is corrupt — reinstall @almadar/core")
    })
}

/// One credential env var a service declares in the services registry.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedServiceCredential {
    #[serde(rename = "envVar")]
    pub env_var: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

/// Service name → declared credential env vars, from the baked services
/// registry. Drives the generated `.env.example` / `SECRETS.md` manifest for
/// the services a schema actually invokes.
pub fn baked_service_credentials() -> &'static HashMap<String, Vec<EmbeddedServiceCredential>> {
    static MAP: OnceLock<HashMap<String, Vec<EmbeddedServiceCredential>>> = OnceLock::new();
    MAP.get_or_init(|| {
        #[derive(Deserialize)]
        struct ServiceEntry {
            #[serde(default)]
            credentials: Vec<EmbeddedServiceCredential>,
        }
        #[derive(Deserialize)]
        struct ServicesRegistry {
            #[serde(default)]
            services: HashMap<String, ServiceEntry>,
        }
        let raw = services_json();
        let registry: ServicesRegistry = serde_json::from_str(raw)
            .expect("installed services-registry.json is corrupt — reinstall @almadar/core");
        registry
            .services
            .into_iter()
            .map(|(name, entry)| (name, entry.credentials))
            .collect()
    })
}
