//! The per-unit sandbox manifest (`docs/Almadar_Sandbox.md` Contracts §2): the grants a deployment unit's orbitals hold,
//! the egress it may reach and the credentials its `http` effects name, with the canonical bytes an Ed25519 signature
//! covers.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::codec::{ed25519_sign, hex};
use crate::os_grants::OsGrants;
use crate::schema::{Orbital, OrbitalSchema, TraitReference};
use crate::secrets::{http_credential_slots, slot_credential};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet};

pub const SUPPORTED_VERSION: u32 = 1;
pub const SIGNATURE_ALG: &str = "ed25519";

/// Why a manifest cannot be built from a program.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestBuildError {
    #[error("MANIFEST_BUILD_UNKNOWN_UNIT: no deployment unit `{unit}` (units: {})", .known.join(", "))]
    UnknownUnit { unit: String, known: Vec<String> },
    #[error("MANIFEST_BUILD_UNKNOWN_ORBITAL: unit `{unit}` lists orbital `{orbital}`, which the program does not declare")]
    UnknownOrbital { unit: String, orbital: String },
    #[error("MANIFEST_BUILD_ARTIFACT_HASH: `{0}` is not 64 lowercase hex characters")]
    BadArtifactHash(String),
    #[error("MANIFEST_BUILD_UNRESOLVED_TRAIT: orbital `{orbital}` still holds the unresolved trait reference `{reference}`")]
    UnresolvedTrait { orbital: String, reference: String },
    #[error("MANIFEST_BUILD_COMPUTED_CREDENTIAL: orbital `{orbital}` names a credential in `{slot}` that is not a literal name")]
    ComputedCredential { orbital: String, slot: String },
    #[error("MANIFEST_BUILD_SERIALIZE: orbital `{orbital}`: {message}")]
    Serialize { orbital: String, message: String },
}

/// The unit a program without `deployment` runs as.
pub const DEFAULT_UNIT: &str = "main";

#[derive(Debug, Clone)]
pub struct Manifest {
    pub version: u32,
    pub unit: String,
    pub program: String,
    pub orbitals: BTreeMap<String, OsGrants>,
    pub grants: OsGrants,
    pub egress: Vec<String>,
    pub ticks: bool,
    /// Per orbital, the credential names it may ask the gate to inject; an orbital with no entry may name none.
    pub credentials: BTreeMap<String, Vec<String>>,
}

/// The JSON text with every object's keys sorted by byte order and no whitespace: the bytes a signature covers.
pub fn canonical_json(value: &Json) -> String {
    match value {
        Json::Object(map) => {
            let mut entries: Vec<(&String, &Json)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(key, entry)| {
                    format!("{}:{}", Json::String(key.clone()), canonical_json(entry))
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Json::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        scalar => scalar.to_string(),
    }
}

fn grants_json(grants: &OsGrants) -> Json {
    let mut fs: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for entry in &grants.fs {
        fs.entry(&entry.glob)
            .or_default()
            .extend(entry.modes.iter().map(|m| m.name()));
    }
    let mut net: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for entry in &grants.net {
        net.entry(&entry.target)
            .or_default()
            .extend(entry.kinds.iter().map(|k| k.name()));
    }
    let exec: BTreeSet<&str> = grants.exec.iter().map(String::as_str).collect();
    let env: BTreeSet<&str> = grants.env.iter().map(String::as_str).collect();
    let stdio: BTreeSet<&str> = grants.stdio.iter().map(|k| k.name()).collect();
    let sys: BTreeSet<&str> = grants.sys.iter().map(|k| k.name()).collect();
    let desktop: BTreeSet<&str> = grants.desktop.iter().map(|k| k.name()).collect();
    let mut json = serde_json::json!({
        "fs": fs, "exec": exec, "net": net, "env": env, "stdio": stdio, "sys": sys, "desktop": desktop,
    });
    if !grants.kv.is_empty() {
        let kv: BTreeMap<&str, BTreeSet<&str>> = grants
            .kv
            .iter()
            .map(|(store, modes)| (store.as_str(), modes.iter().map(|m| m.name()).collect()))
            .collect();
        json["kv"] = serde_json::json!(kv);
    }
    json
}

impl Manifest {
    /// The manifest as JSON without a `signature`: what `sign_manifest` takes and what a signature covers.
    pub fn to_unsigned_json(&self) -> Json {
        let orbitals: BTreeMap<&str, Json> = self
            .orbitals
            .iter()
            .map(|(name, grants)| (name.as_str(), grants_json(grants)))
            .collect();
        serde_json::json!({
            "version": self.version, "unit": self.unit, "program": self.program,
            "orbitals": orbitals, "grants": grants_json(&self.grants),
            "egress": self.egress, "ticks": self.ticks, "credentials": self.credentials,
        })
    }

    /// The manifest for deployment unit `unit` of `schema`, unsigned (`sign_manifest` signs `to_unsigned_json`). `artifact_sha256` is the lowercase
    /// hex digest of the program artifact the run zone will load. A program without `deployment` is the one unit
    /// `main` holding every orbital.
    pub fn build(
        schema: &OrbitalSchema,
        unit: &str,
        artifact_sha256: &str,
    ) -> Result<Manifest, ManifestBuildError> {
        if artifact_sha256.len() != 64
            || !artifact_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ManifestBuildError::BadArtifactHash(
                artifact_sha256.to_string(),
            ));
        }
        let members = unit_members(schema, unit)?;
        let mut orbitals = BTreeMap::new();
        let mut credentials = BTreeMap::new();
        let mut union = OsGrants::default();
        let mut ticks = false;
        for orbital in members {
            let grants = schema.orbital_os_grants(orbital);
            union.union(&grants);
            orbitals.insert(orbital.name.clone(), grants);
            let scan = scan_orbital(orbital)?;
            ticks |= scan.ticks;
            if !scan.credentials.is_empty() {
                credentials.insert(orbital.name.clone(), scan.credentials.into_iter().collect());
            }
        }
        let egress: BTreeSet<String> = schema.egress.iter().flatten().cloned().collect();
        let manifest = Manifest {
            version: SUPPORTED_VERSION,
            unit: unit.to_string(),
            program: format!("sha256:{artifact_sha256}"),
            orbitals,
            grants: union,
            egress: egress.into_iter().collect(),
            ticks,
            credentials,
        };
        Ok(manifest)
    }
}

/// The deployment units of `schema`: the `deployment` keys, or the one unit `main`.
pub fn unit_names(schema: &OrbitalSchema) -> Vec<String> {
    match &schema.deployment {
        Some(units) => units.keys().cloned().collect(),
        None => vec![DEFAULT_UNIT.to_string()],
    }
}

fn unit_members<'a>(
    schema: &'a OrbitalSchema,
    unit: &str,
) -> Result<Vec<&'a Orbital>, ManifestBuildError> {
    let unknown = || ManifestBuildError::UnknownUnit {
        unit: unit.to_string(),
        known: unit_names(schema),
    };
    match &schema.deployment {
        Some(units) => units
            .get(unit)
            .ok_or_else(unknown)?
            .iter()
            .map(|name| {
                schema
                    .orbitals
                    .iter()
                    .find(|orbital| orbital.name == *name)
                    .ok_or_else(|| ManifestBuildError::UnknownOrbital {
                        unit: unit.to_string(),
                        orbital: name.clone(),
                    })
            })
            .collect(),
        None if unit == DEFAULT_UNIT => Ok(schema.orbitals.iter().collect()),
        None => Err(unknown()),
    }
}

struct OrbitalScan {
    ticks: bool,
    credentials: BTreeSet<String>,
}

/// What one orbital's traits declare: whether any has `ticks {}`, and the credential names its `http` effects
/// reference. A credential slot that is not a literal name is an error; nothing is guessed.
fn scan_orbital(orbital: &Orbital) -> Result<OrbitalScan, ManifestBuildError> {
    let mut scan = OrbitalScan {
        ticks: false,
        credentials: BTreeSet::new(),
    };
    for reference in &orbital.traits {
        let TraitReference::Inline(definition) = reference else {
            return Err(ManifestBuildError::UnresolvedTrait {
                orbital: orbital.name.clone(),
                reference: reference.trait_name().unwrap_or_default().to_string(),
            });
        };
        scan.ticks |= definition.ticks.as_ref().is_some_and(|t| !t.is_empty());
        let value =
            serde_json::to_value(definition).map_err(|e| ManifestBuildError::Serialize {
                orbital: orbital.name.clone(),
                message: e.to_string(),
            })?;
        for (slot, slot_value) in http_credential_slots(&value) {
            match slot_credential(slot_value) {
                Some(name) if !name.is_empty() && !name.starts_with('@') => {
                    scan.credentials.insert(name.to_string());
                }
                _ => {
                    return Err(ManifestBuildError::ComputedCredential {
                        orbital: orbital.name.clone(),
                        slot,
                    })
                }
            }
        }
    }
    Ok(scan)
}

/// A signing seed file's text: 64 hex characters, surrounding whitespace ignored.
pub fn parse_seed(text: &str) -> Option<[u8; 32]> {
    crate::codec::hex_decode(text.trim())?.try_into().ok()
}

/// The lower-case hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&crate::codec::sha256(bytes))
}

/// The manifest JSON (no `signature` key) signed with `seed`; returns the full manifest text.
pub fn sign_manifest(unsigned: &Json, key_id: &str, seed: &[u8; 32]) -> Result<String, String> {
    let Some(object) = unsigned.as_object() else {
        return Err("the manifest must be a JSON object".to_string());
    };
    if object.contains_key("signature") {
        return Err("the manifest to sign already carries a signature".to_string());
    }
    let sig = ed25519_sign(seed, canonical_json(unsigned).as_bytes());
    let mut signed = object.clone();
    signed.insert(
        "signature".to_string(),
        serde_json::json!({ "alg": SIGNATURE_ALG, "key_id": key_id, "sig": STANDARD.encode(sig) }),
    );
    Ok(Json::Object(signed).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{ed25519_public_key, ed25519_verify};

    const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";

    fn signed_bytes_and_signature(text: &str) -> (Vec<u8>, [u8; 64]) {
        let mut value: Json = serde_json::from_str(text).unwrap();
        let signature = value.as_object_mut().unwrap().remove("signature").unwrap();
        assert_eq!(signature["alg"], SIGNATURE_ALG);
        assert_eq!(signature["key_id"], "deploy-1");
        let sig = STANDARD.decode(signature["sig"].as_str().unwrap()).unwrap();
        (canonical_json(&value).into_bytes(), sig.try_into().unwrap())
    }

    #[test]
    fn a_signed_manifest_verifies_under_the_seeds_public_key_and_any_change_to_it_does_not() {
        let seed = parse_seed(&format!("  {SEED}\n")).unwrap();
        let unsigned = serde_json::json!({ "version": 1, "unit": "main", "ticks": false, "egress": ["a.example"] });
        let signed = sign_manifest(&unsigned, "deploy-1", &seed).unwrap();
        let public = ed25519_public_key(&seed);
        let (bytes, signature) = signed_bytes_and_signature(&signed);
        assert!(ed25519_verify(&public, &bytes, &signature));

        let tampered = signed.replace("a.example", "b.example");
        let (bytes, signature) = signed_bytes_and_signature(&tampered);
        assert!(!ed25519_verify(&public, &bytes, &signature), "control: a changed egress breaks the signature");
        let (bytes, signature) = signed_bytes_and_signature(&signed);
        let mut other = seed;
        other[0] ^= 1;
        assert!(!ed25519_verify(&ed25519_public_key(&other), &bytes, &signature), "control: another key does not verify it");
    }

    #[test]
    fn signing_refuses_a_non_object_and_a_manifest_that_is_already_signed_and_a_seed_must_be_64_hex() {
        let seed = parse_seed(SEED).unwrap();
        assert!(sign_manifest(&serde_json::json!([1]), "k", &seed).is_err());
        assert!(sign_manifest(&serde_json::json!({ "signature": {} }), "k", &seed).is_err());
        assert!(parse_seed("abcd").is_none());
        assert!(parse_seed(&SEED.replace('9', "z")).is_none());
    }

    #[test]
    fn the_canonical_form_sorts_keys_and_sha256_hex_is_lower_case_hex() {
        assert_eq!(canonical_json(&serde_json::json!({ "b": [2, { "z": 1, "a": 0 }], "a": null })), r#"{"a":null,"b":[2,{"a":0,"z":1}]}"#);
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn a_program_without_deployment_is_the_one_unit_main_and_a_bad_artifact_hash_or_unit_is_refused() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({ "name": "App", "orbitals": [] })).unwrap();
        assert_eq!(unit_names(&schema), vec!["main".to_string()]);
        let digest = sha256_hex(b"x");
        let manifest = Manifest::build(&schema, "main", &digest).unwrap();
        assert_eq!(manifest.program, format!("sha256:{digest}"));
        assert!(matches!(Manifest::build(&schema, "main", "xyz"), Err(ManifestBuildError::BadArtifactHash(_))));
        assert!(matches!(Manifest::build(&schema, "other", &digest), Err(ManifestBuildError::UnknownUnit { .. })));
    }
}
