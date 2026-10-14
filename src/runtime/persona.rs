//! The dev viewer — who `@user.x` resolves to when no HTTP auth ran.
//!
//! `orb verify`, the client-render probe and the screenshotter all evaluate guards
//! and fetch filters with no request behind them, so without a viewer every
//! `@user.role` gate takes its negative branch and every ownership filter matches
//! nothing — silently, and identically to a genuinely-broken gate. `ALMADAR_PERSONA`
//! names the viewer so a behavior can be verified per persona.
//!
//! There is no global persona roster: personas are declared PER APP as the seeded
//! rows of its `[identity]` entity (`Almadar_LOLO_Identity.md` §4.3), derived by
//! [`identity_roster`]. A bare id/role spec therefore resolves only once a schema
//! is in hand; the self-contained JSON form resolves anywhere.
//!
//! **Cross-language mirror:** resolution semantics must stay in step with
//! `resolvePersonaSpec` / `findPersonaInRoster` in
//! `packages/almadar-core/src/types/user.ts`; the roster derivation is pinned to
//! the interpreter path by the shared fixtures in `packages/almadar-parity`.

use super::mock_random::iso_from_ms;
use super::seed::{identity_entity_def, identity_roster_rows, instance_rows};
use crate::schema::OrbitalSchema;
use crate::Value;

/// The env var naming the viewer, shared with the interpreter path.
pub const PERSONA_ENV: &str = "ALMADAR_PERSONA";

/// Roster rows derived when a host names no seed count. Mirrors the interpreter
/// path's `MockPersistenceAdapter` default (`defaultSeedCount: 6`) — enough rows
/// for the union cycler to cover every declared role of a typical entity.
pub const DEFAULT_IDENTITY_ROSTER_COUNT: usize = 6;

/// The `ALMADAR_PERSONA` spec, classified by what it needs to resolve.
pub enum EnvPersona {
    /// Env var unset or blank — no viewer named.
    None,
    /// Self-contained JSON `UserContext` — resolved right here.
    Resolved(Value),
    /// A bare id/role — resolvable only against an app's declared roster.
    Bare(String),
}

/// Read and classify `ALMADAR_PERSONA`. `Err` only for a malformed JSON form —
/// a developer who asked to be somebody must never be shown the app as nobody.
pub fn persona_from_env_spec() -> Result<EnvPersona, String> {
    let raw = match std::env::var(PERSONA_ENV) {
        Ok(raw) if !raw.trim().is_empty() => raw,
        _ => return Ok(EnvPersona::None),
    };
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        return resolve_persona_spec(trimmed, &[]).map(EnvPersona::Resolved);
    }
    Ok(EnvPersona::Bare(trimmed.to_string()))
}

fn str_field<'a>(persona: &'a Value, key: &str) -> Option<&'a str> {
    match persona.get_path(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
        _ => None,
    }
}

/// Look a persona up in a roster by `id`, then by `role`.
pub fn find_persona_in_roster<'a>(roster: &'a [Value], id_or_role: &str) -> Option<&'a Value> {
    roster
        .iter()
        .find(|p| str_field(p, "id") == Some(id_or_role))
        .or_else(|| {
            roster
                .iter()
                .find(|p| str_field(p, "role") == Some(id_or_role))
        })
}

/// The app's declared persona roster: the seeded rows of its `[identity]` entity.
///
/// Authored `instances` ARE the roster (materialized as the kernel stores them);
/// otherwise the rows are the identity entity's seeded rows, the first rows of every
/// seeded mock store ([`identity_roster_rows`]), so each persona id is a real row.
/// Empty when the schema declares no `[identity]` entity — there is no global
/// fallback roster by design.
pub fn identity_roster(schema: &OrbitalSchema, count: usize) -> Vec<Value> {
    let Some(def) = identity_entity_def(schema) else {
        return Vec::new();
    };
    let now = crate::time_util::now_millis_f64();
    if let Some(instances) = def.instances.as_ref().filter(|list| !list.is_empty()) {
        return instance_rows(&def.name, instances, &iso_from_ms(now));
    }
    identity_roster_rows(schema, count, now)
}

/// Resolve a persona spec — a bare declared id/role, or a full JSON user object —
/// against the app's declared roster.
///
/// `Err` on anything unresolvable: "no persona" and "persona silently ignored"
/// look identical on screen, so a bad spec must be reported rather than degraded
/// into an anonymous viewer.
pub fn resolve_persona_spec(spec: &str, roster: &[Value]) -> Result<Value, String> {
    let raw = spec.trim();
    if !raw.starts_with('{') {
        return find_persona_in_roster(roster, raw).cloned().ok_or_else(|| {
            if roster.is_empty() {
                format!(
                    "Persona \"{raw}\" is not a declared persona id or role. This app declares no [identity] entity, so only the JSON persona form is accepted."
                )
            } else {
                format!(
                    "Persona \"{}\" is not a declared persona id or role. Known: {}",
                    raw,
                    roster
                        .iter()
                        .map(|p| format!(
                            "{}/{}",
                            str_field(p, "id").unwrap_or("-"),
                            str_field(p, "role").unwrap_or("-")
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        });
    }
    let user: Value = serde_json::from_str(raw)
        .map_err(|e| format!("Persona is not valid JSON: {} ({})", raw, e))?;
    let has_id = match user.as_object() {
        Some(map) => matches!(
            map.get("id").or_else(|| map.get("uid")),
            Some(Value::String(s)) if !s.is_empty()
        ),
        None => false,
    };
    if !has_id {
        return Err(format!("Persona needs an \"id\" (or \"uid\"): {}", raw));
    }
    Ok(user)
}

/// The default viewer an L2 probe should bind when `ALMADAR_PERSONA` names none: the
/// first declared `[identity]` roster persona, deterministic roster order — never a
/// heuristic ranking of roles. `None` both when the schema declares no roster and when
/// `ALMADAR_PERSONA` IS set (an explicit env persona wins; the kernel resolves it at
/// `register_schema` and the unstamped seeder reads it directly, so a caller must not
/// double-bind here). Shared by every probe bridge (in-process kernel, HTTP server) so
/// an `[identity]`-declaring app is never evaluated anonymously in one but not the other.
pub fn default_probe_persona(schema: &OrbitalSchema) -> Option<Value> {
    if !matches!(persona_from_env_spec(), Ok(EnvPersona::None)) {
        return None;
    }
    identity_roster(schema, DEFAULT_IDENTITY_ROSTER_COUNT)
        .first()
        .cloned()
}

/// The viewer named by `ALMADAR_PERSONA` resolved against `schema`'s declared
/// roster, or `Value::Null` when unset.
pub fn persona_from_env_for_schema(schema: &OrbitalSchema) -> Result<Value, String> {
    match persona_from_env_spec()? {
        EnvPersona::None => Ok(Value::Null),
        EnvPersona::Resolved(user) => Ok(user),
        EnvPersona::Bare(spec) => resolve_persona_spec(
            &spec,
            &identity_roster(schema, DEFAULT_IDENTITY_ROSTER_COUNT),
        ),
    }
}

#[cfg(test)]
mod tests {
    use crate::evaluator::ValueMap;
    use super::*;
    use serde_json::json;
    

    fn roster3() -> Vec<Value> {
        ["member", "moderator", "admin"]
            .iter()
            .enumerate()
            .map(|(i, role)| {
                let mut m = ValueMap::new();
                m.insert("id".to_string(), Value::String(format!("Person-{}", i + 1)));
                m.insert("role".to_string(), Value::String((*role).to_string()));
                Value::Object(m)
            })
            .collect()
    }

    /// Built through serde so the test exercises the same wire shape a real
    /// `.orb` carries (mirrors the seed.rs test fixtures).
    fn identity_schema(extra_entity_json: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "PersonOrbital",
                "entity": extra_entity_json,
                "traits": [],
                "pages": []
            }]
        }))
        .expect("schema fixture must deserialize")
    }

    #[test]
    fn resolves_a_bare_id_and_role_against_the_roster() {
        let roster = roster3();
        let by_id = resolve_persona_spec("Person-2", &roster).expect("declared id");
        let by_role = resolve_persona_spec(" moderator ", &roster).expect("declared role");
        assert_eq!(by_id, by_role);
        assert_eq!(
            by_id.get_path("role"),
            Some(&Value::String("moderator".to_string()))
        );
    }

    #[test]
    fn resolves_full_json_without_a_roster() {
        let user =
            resolve_persona_spec("{\"id\":\"patient-7\",\"role\":\"patient\"}", &[]).unwrap();
        assert_eq!(
            user.get_path("role"),
            Some(&Value::String("patient".to_string()))
        );
    }

    #[test]
    fn rejects_unresolvable_specs() {
        let roster = roster3();
        assert!(resolve_persona_spec("who-dis", &roster).is_err());
        assert!(resolve_persona_spec("{oops", &roster).is_err());
        assert!(resolve_persona_spec("{\"role\":\"admin\"}", &roster).is_err());
        let empty_err = resolve_persona_spec("member", &[]).unwrap_err();
        assert!(empty_err.contains("no [identity] entity"));
    }

    #[test]
    fn derives_the_roster_from_the_identity_entity() {
        let schema = identity_schema(json!({
            "name": "Person",
            "identity": true,
            "persistence": "persistent",
            "fields": [
                { "name": "id", "type": "string", "required": true },
                { "name": "name", "type": "string" },
                { "name": "role", "type": "string", "values": ["member", "moderator", "admin"] }
            ]
        }));
        let roster = identity_roster(&schema, 3);
        assert_eq!(roster.len(), 3);
        // Every persona carries its role: an optional field on the `[identity]` entity is
        // never left unset, so each declared role is a real, selectable viewer.
        let roles: Vec<_> = roster.iter().map(|p| p.get_path("role").cloned()).collect();
        assert_eq!(
            roles,
            vec![
                Some(Value::String("member".to_string())),
                Some(Value::String("moderator".to_string())),
                Some(Value::String("admin".to_string())),
            ]
        );
        assert_eq!(
            roster[0].get_path("id"),
            Some(&Value::String("Person Id 1".to_string()))
        );
    }

    #[test]
    fn authored_instances_are_the_roster() {
        let schema = identity_schema(json!({
            "name": "Person",
            "identity": true,
            "persistence": "persistent",
            "fields": [
                { "name": "id", "type": "string", "required": true },
                { "name": "name", "type": "string" },
                { "name": "role", "type": "string", "values": ["member", "admin"] }
            ],
            "instances": [
                { "id": "ada", "name": "Ada", "role": "admin" },
                { "name": "Sam", "role": "member" }
            ]
        }));
        let roster = identity_roster(&schema, 6);
        assert_eq!(roster.len(), 2);
        assert_eq!(
            roster[0].get_path("id"),
            Some(&Value::String("ada".to_string()))
        );
        // An instance without an authored id gets the mock id family, counted over
        // id-less instances only (as the runtime's mock store mints it).
        assert_eq!(
            roster[1].get_path("id"),
            Some(&Value::String("Person Id 1".to_string()))
        );
    }

    #[test]
    fn no_identity_entity_means_an_empty_roster() {
        let schema = identity_schema(json!({
            "name": "Person",
            "persistence": "persistent",
            "fields": [{ "name": "id", "type": "string", "required": true }]
        }));
        assert!(identity_roster(&schema, 6).is_empty());
    }
}
