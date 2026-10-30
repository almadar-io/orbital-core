//! `contractQuery`: the declared-contract facts a host reads off the registered program, one tagged request and one
//! JSON answer. A host asks the kernel which external inputs the program declares, which entities a provider may read,
//! which handler an inbound hook names, what an inbound delivery's payload and answer are, which orbital owns an
//! entity, and which rows a viewer may read: it carries none of these rules itself (EXEC-BOUND).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::*;
use orbital_core_base::external_inputs::{find_external_input, readable_entities_of};
use orbital_core_base::inbound::{
    answer_of_response, credential_unset, dispatch_failed, inbound_only_message, inbound_payload,
    InboundRoute,
};
use orbital_core_base::payload_check::{
    find_input_by_address, payload_issues, payload_json_schema,
};
use orbital_core_base::schema::types::{EntityRef, PayloadField};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An event a dispatch emitted, as a host reads it off the response.
#[derive(Debug, Deserialize)]
pub struct EmittedEventQuery {
    pub event: String,
    #[serde(default)]
    pub payload: Option<Value>,
}

/// The trait an address is resolved from.
#[derive(Debug, Deserialize)]
pub struct InputCaller {
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_name: String,
}

/// One question about the registered program. Answered by [`RuntimeKernel::contract_query`].
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ContractQuery {
    /// Every declared external input.
    ExternalInputs,
    /// The entities a provider acting as the caller may read.
    ReadableEntities,
    /// The declared input of `trait.event` in `orbital`, or null.
    #[serde(rename_all = "camelCase")]
    FindExternalInput {
        orbital: String,
        #[serde(rename = "trait")]
        trait_name: String,
        event: String,
    },
    /// The declared input a `listens`-addressed event names from the calling trait's position, or null.
    FindInputByAddress {
        address: String,
        caller: InputCaller,
    },
    /// A declared payload as the JSON Schema of one object.
    PayloadSchemaToJsonSchema { fields: Vec<PayloadField> },
    /// The ways `value` misses the declared payload `fields`.
    PayloadIssues {
        fields: Vec<PayloadField>,
        value: serde_json::Value,
    },
    /// `{ route }` for a hook name the program serves, else `{ answer }`.
    FindInbound { name: String },
    /// The trait handling `event` as an inbound hook in `orbital`, with the refusal message, or null.
    #[serde(rename_all = "camelCase")]
    InboundOnly {
        orbital: String,
        #[serde(rename = "trait", default)]
        trait_name: Option<String>,
        event: String,
    },
    /// The payload an inbound event is delivered with.
    #[serde(rename_all = "camelCase")]
    InboundPayload {
        route: InboundRoute,
        headers: BTreeMap<String, String>,
        raw_body: String,
        #[serde(default)]
        credential: Option<String>,
    },
    /// The HTTP answer a finished dispatch produced for `route`.
    #[serde(rename_all = "camelCase")]
    InboundAnswer {
        route: InboundRoute,
        emitted: Vec<EmittedEventQuery>,
        #[serde(default)]
        error: Option<String>,
    },
    /// The answer for a route whose credential the host holds no value for.
    CredentialUnset { credential: String },
    /// The answer for a delivery whose dispatch failed.
    DispatchFailed { message: String },
    /// The name of the `[identity]` entity, or null.
    IdentityEntity,
    /// The orbital that declares the entity inline, or null.
    EntityOwner { entity: String },
    /// What a host reads of one trait: owner, entity, states, events, listens and transitions.
    TraitSurface {
        #[serde(rename = "trait")]
        trait_name: String,
    },
    /// Every registered trait's surface and the entities whose rows live in the browser.
    ProgramSurface,
    /// `rows` of `entity` as `viewer` reads them: narrowed by the entity's declared `@read`, `@user` bound to the viewer
    /// (`null`: no viewer).
    ReadRows {
        entity: String,
        rows: Vec<Value>,
        viewer: serde_json::Value,
    },
}

/// The emitter a listen hears, its orbital resolved: the declared one, else the listening trait's own.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SurfaceListenSource {
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_name: String,
}

/// One `listens` entry; `source` is `None` for a listen that hears any emitter.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SurfaceListen {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SurfaceListenSource>,
}

/// One transition per source state.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SurfaceTransition {
    pub from: String,
    pub to: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub guard: Option<serde_json::Value>,
    #[serde(rename = "awaitRender", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "JsonValue", optional))]
    pub await_render: Option<Vec<orbital_core_base::ir::types::AwaitRender>>,
}

/// One declared tick of a trait: its name and the run-mode flags the kernel holds.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceTick {
    pub name: String,
    pub while_visible: bool,
    pub runs_in_background: bool,
}

/// One event a trait declares it emits, with the id the schema stamps on it.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceEmit {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

/// What a host reads of one registered trait. `events` are the transition event keys without the lifecycle events
/// (`INIT`, `LOAD`, `$MOUNT`), deduplicated in declaration order.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitSurface {
    pub name: String,
    pub orbital: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orbital_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trait_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    pub shared: bool,
    pub browser: bool,
    pub frame_key: String,
    pub ticks: Vec<SurfaceTick>,
    pub states: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial: Option<String>,
    pub events: Vec<String>,
    pub emits: Vec<SurfaceEmit>,
    pub listens: Vec<SurfaceListen>,
    pub transitions: Vec<SurfaceTransition>,
}

/// What a host reads of the whole program: every registered trait in name order, and the browser-stored entities.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramSurface {
    pub traits: Vec<TraitSurface>,
    pub browser_entities: Vec<String>,
}

fn json<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|e| format!("serialize: {e}"))
}

impl RuntimeKernel {
    /// The answer to one [`ContractQuery`], as JSON.
    pub fn contract_query(&self, query: ContractQuery) -> Result<serde_json::Value, String> {
        match query {
            ContractQuery::ExternalInputs => json(&self.external_inputs()),
            ContractQuery::ReadableEntities => {
                let mut names: Vec<String> = Vec::new();
                for schema in self.registered_programs() {
                    for name in readable_entities_of(&schema) {
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                }
                json(&names)
            }
            ContractQuery::FindExternalInput {
                orbital,
                trait_name,
                event,
            } => json(
                &self
                    .registered_programs()
                    .iter()
                    .find_map(|schema| find_external_input(schema, &orbital, &trait_name, &event)),
            ),
            ContractQuery::FindInputByAddress { address, caller } => {
                let found = self.registered_programs().iter().find_map(|schema| {
                    let inputs = orbital_core_base::external_inputs::external_inputs_of(schema);
                    find_input_by_address(&inputs, &address, &caller.orbital, &caller.trait_name)
                        .cloned()
                });
                json(&found)
            }
            ContractQuery::PayloadSchemaToJsonSchema { fields } => Ok(payload_json_schema(&fields)),
            ContractQuery::PayloadIssues { fields, value } => {
                json(&payload_issues(&fields, &value))
            }
            ContractQuery::FindInbound { name } => match self.inbound_route(&name) {
                Ok(route) => json(&serde_json::json!({ "route": route })),
                Err(answer) => json(&serde_json::json!({ "answer": answer })),
            },
            ContractQuery::InboundOnly {
                orbital,
                trait_name,
                event,
            } => json(
                &self
                    .inbound_only_trait(&orbital, trait_name.as_deref(), &event)
                    .map(|handler| {
                        let message = inbound_only_message(&handler, &event);
                        serde_json::json!({ "trait": handler, "message": message })
                    }),
            ),
            ContractQuery::InboundPayload {
                route,
                headers,
                raw_body,
                credential,
            } => json(&inbound_payload(
                &route,
                headers.iter().map(|(k, v)| (k.as_str(), v.as_str())),
                &raw_body,
                credential.as_deref(),
            )),
            ContractQuery::InboundAnswer {
                route,
                emitted,
                error,
            } => {
                let empty = Value::object(orbital_core_base::value::ValueMap::default());
                let emits: Vec<(&str, &Value)> = emitted
                    .iter()
                    .map(|e| (e.event.as_str(), e.payload.as_ref().unwrap_or(&empty)))
                    .collect();
                json(&answer_of_response(&route, emits, error.as_deref()))
            }
            ContractQuery::CredentialUnset { credential } => json(&credential_unset(&credential)),
            ContractQuery::DispatchFailed { message } => json(&dispatch_failed(&message)),
            ContractQuery::IdentityEntity => {
                json(&self.registered_programs().iter().find_map(|schema| {
                    orbital_core_base::policy::identity_entity_name(schema).map(str::to_string)
                }))
            }
            ContractQuery::EntityOwner { entity } => json(&self.entity_owner_orbital(&entity)),
            ContractQuery::TraitSurface { trait_name } => json(&self.trait_surface(&trait_name)?),
            ContractQuery::ProgramSurface => json(&self.program_surface()?),
            ContractQuery::ReadRows {
                entity,
                rows,
                viewer,
            } => {
                let bindings = if viewer.is_null() {
                    serde_json::json!({ "state": "idle" })
                } else {
                    serde_json::json!({ "user": viewer, "state": "idle" })
                };
                let policy = self
                    .registry
                    .entity_policies
                    .read()
                    .get(&entity)
                    .and_then(|p| p.read.clone());
                let policy_json = match &policy {
                    Some(policy) => Some(
                        serde_json::to_string(policy)
                            .map_err(|e| format!("serialize policy: {e}"))?,
                    ),
                    None => None,
                };
                let scoped = crate::embed::eval_wire::scope_rows_json(
                    &bindings.to_string(),
                    &serde_json::to_string(&rows).map_err(|e| format!("serialize rows: {e}"))?,
                    policy_json.as_deref(),
                    None,
                    &entity,
                )?;
                serde_json::from_str(&scoped).map_err(|e| format!("parse scoped rows: {e}"))
            }
        }
    }

    /// The surface of the registered trait `trait_name`; an unknown or ambiguous trait is an error.
    pub fn trait_surface(&self, trait_name: &str) -> Result<TraitSurface, String> {
        use crate::effects::response::LifecycleEvent;
        let owners = crate::trait_owners::TraitOwners::from_kernel(self);
        let orbital_name = owners.owning_orbital(trait_name)?;
        let def = self
            .registry
            .trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| format!("unknown trait: {trait_name}"))?;
        let programs = self.registered_programs();
        let orbital = programs
            .iter()
            .flat_map(|schema| schema.orbitals.iter())
            .find(|orbital| orbital.name == orbital_name)
            .ok_or_else(|| format!("unknown orbital: {orbital_name}"))?;
        let entity = def.linked_entity.clone().or_else(|| match &orbital.entity {
            EntityRef::Inline(e) => Some(e.name.clone()),
            EntityRef::Reference(r) => Some(r.clone()),
            EntityRef::Extends(e) => Some(e.reference.clone()),
        });
        let shared = entity.as_deref().is_some_and(|name| {
            programs
                .iter()
                .flat_map(|schema| schema.orbitals.iter())
                .any(|o| {
                    std::iter::once(&o.entity)
                        .chain(o.auxiliary_entities.iter().flatten())
                        .any(|er| matches!(er, EntityRef::Inline(d) if d.name == name && d.shared))
                })
        });
        let browser = entity.as_deref().is_some_and(|name| {
            self.registry
                .entity_definitions
                .read()
                .get(name)
                .is_some_and(|d| d.browser)
        });
        let ticks = def
            .ticks
            .iter()
            .flatten()
            .map(|t| SurfaceTick {
                name: t.name.clone(),
                while_visible: t.while_visible,
                runs_in_background: t.background,
            })
            .collect();
        let machine = &def.state_machine;
        let mut events: Vec<String> = Vec::new();
        let mut transitions = Vec::new();
        for t in &machine.transitions {
            let guard = t
                .guard
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| format!("guard of {} on {}: {e}", t.event, trait_name))?;
            if LifecycleEvent::parse(&t.event).is_none() && !events.contains(&t.event) {
                events.push(t.event.clone());
            }
            for from in t.from.states() {
                transitions.push(SurfaceTransition {
                    from: from.to_string(),
                    to: t.to.clone(),
                    event: t.event.clone(),
                    guard: guard.clone(),
                    await_render: t.await_render.clone(),
                });
            }
        }
        let listens = def
            .listens
            .iter()
            .flat_map(|listens| listens.iter())
            .map(|listen| SurfaceListen {
                event: listen.event.clone(),
                source: match &listen.source {
                    None | Some(orbital_core_base::schema::ListenSource::Any) => None,
                    Some(orbital_core_base::schema::ListenSource::Trait { trait_name, .. }) => {
                        Some(SurfaceListenSource {
                            orbital: orbital_name.to_string(),
                            trait_name: trait_name.clone(),
                        })
                    }
                    Some(orbital_core_base::schema::ListenSource::Orbital {
                        orbital,
                        trait_name,
                        ..
                    }) => Some(SurfaceListenSource {
                        orbital: orbital.clone(),
                        trait_name: trait_name.clone(),
                    }),
                },
            })
            .collect();
        let identity = self.emit_source(trait_name);
        let emits = def
            .emits
            .iter()
            .flatten()
            .map(|e| SurfaceEmit {
                event: e.event.clone(),
                event_id: e.event_id.clone(),
            })
            .collect();
        Ok(TraitSurface {
            name: trait_name.to_string(),
            orbital: orbital_name.to_string(),
            orbital_id: identity.orbital_id,
            trait_id: identity.trait_id,
            entity,
            shared,
            browser,
            frame_key: self.trait_frame_key(trait_name),
            ticks,
            states: machine.states.iter().map(|s| s.name.clone()).collect(),
            initial: machine
                .states
                .iter()
                .find(|s| s.is_initial == Some(true))
                .map(|s| s.name.clone()),
            events,
            emits,
            listens,
            transitions,
        })
    }

    /// Every registered trait's surface in trait-name order; an ambiguous trait is an error.
    pub fn program_surface(&self) -> Result<ProgramSurface, String> {
        let owners = crate::trait_owners::TraitOwners::from_kernel(self);
        let traits = owners
            .iter()
            .map(|(name, _)| self.trait_surface(name))
            .collect::<Result<Vec<_>, _>>()?;
        let mut browser_entities: Vec<String> = self
            .registry
            .entity_definitions
            .read()
            .iter()
            .filter(|(_, d)| d.browser)
            .map(|(name, _)| name.clone())
            .collect();
        browser_entities.sort();
        Ok(ProgramSurface {
            traits,
            browser_entities,
        })
    }

    /// The orbital, across every registered program, that declares `entity` inline (its primary entity or an auxiliary).
    pub fn entity_owner_orbital(&self, entity: &str) -> Option<String> {
        self.registered_programs().iter().find_map(|schema| {
            schema.orbitals.iter().find_map(|orbital| {
                std::iter::once(&orbital.entity)
                    .chain(orbital.auxiliary_entities.iter().flatten())
                    .any(|er| matches!(er, EntityRef::Inline(def) if def.name == entity))
                    .then(|| orbital.name.clone())
            })
        })
    }
}

/// The answer to a [`ContractQuery`] for a host with no registered program: the kernel's own answers on an empty
/// kernel (EXEC-INBOUND-1: an unregistered hook is `unknown_hook`).
pub fn contract_query_without_program(query: ContractQuery) -> Result<serde_json::Value, String> {
    RuntimeKernel::new(
        orbital_core_base::effect_types::RuntimeEnvironment::Server,
        std::sync::Arc::new(crate::entity_source::InMemoryEntityCache::new()),
        crate::config::KernelConfig::default(),
    )
    .contract_query(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_source::InMemoryEntityCache;
    use crate::kernel::{KernelConfig, RuntimeEnvironment};
    use serde_json::json;

    fn kernel() -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "inbound": ["hook"],
            "typeContractVersion": 1,
            "orbitals": [{
                "name": "Cal",
                "entity": { "name": "Note", "collection": "notes", "fields": [{ "name": "id", "type": "string" }, { "name": "owner", "type": "string" }],
                            "read_policy": ["=", "@entity.owner", "@user.id"] },
                "auxiliaryEntities": [{ "name": "Scratch", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] }],
                "traits": [{
                    "name": "Push",
                    "linkedEntity": "Note",
                    "emits": [{ "event": "ANSWERED" }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [
                            { "key": "PUSH", "inbound": { "name": "hook", "answers": "ANSWERED" } },
                            { "key": "ADD", "input": true, "payloadSchema": [{ "name": "title", "type": "string", "required": true }] }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "PUSH", "effects": [["emit", "ANSWERED", { "status": 200, "body": "ok" }]] },
                            { "from": "idle", "to": "idle", "event": "ADD" }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        kernel.register_schema(schema).unwrap();
        kernel
    }

    fn ask(k: &RuntimeKernel, query: serde_json::Value) -> serde_json::Value {
        k.contract_query(serde_json::from_value(query).expect("query"))
            .expect("answer")
    }

    fn await_render() -> serde_json::Value {
        json!({ "trait": "Panel", "slot": "main", "skeleton": { "shape": { "variant": "list", "rows": 3 } } })
    }

    fn surface_kernel() -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1,
            "orbitals": [{
                "name": "Door",
                "id": "orb-door",
                "entity": { "name": "Lock", "collection": "locks", "shared": true, "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }] },
                "traits": [{
                    "name": "Gate",
                    "linkedEntity": "Lock",
                    "ticks": [
                        { "name": "poll", "interval": 1000, "effects": [], "whileVisible": true },
                        { "name": "sweep", "interval": "5s", "effects": [], "runsInBackground": true }
                    ],
                    "listens": [{ "event": "PING", "triggers": "OPEN", "source": { "kind": "trait", "trait": "Bell" } }],
                    "stateMachine": {
                        "states": [{ "name": "shut", "isInitial": true }, { "name": "open" }],
                        "events": [{ "key": "OPEN" }, { "key": "INIT" }],
                        "transitions": [
                            { "from": "shut", "to": "open", "event": "OPEN", "guard": [">", "@entity.n", 1.0], "awaitRender": [await_render()] },
                            { "from": ["open"], "to": "shut", "event": "INIT" }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        kernel.register_schema(schema).unwrap();
        kernel
    }

    #[test]
    fn a_trait_surface_is_what_the_kernel_holds_of_the_trait() {
        let k = surface_kernel();
        assert_eq!(
            ask(&k, json!({ "kind": "traitSurface", "trait": "Gate" })),
            json!({
                "name": "Gate",
                "orbital": "Door",
                "orbitalId": "orb-door",
                "entity": "Lock",
                "shared": true,
                "browser": false,
                "frameKey": "$shared::Lock",
                "ticks": [
                    { "name": "poll", "whileVisible": true, "runsInBackground": false },
                    { "name": "sweep", "whileVisible": false, "runsInBackground": true }
                ],
                "states": ["shut", "open"],
                "initial": "shut",
                "events": ["OPEN"],
                "emits": [],
                "listens": [{ "event": "PING", "source": { "orbital": "Door", "trait": "Bell" } }],
                "transitions": [
                    { "from": "shut", "to": "open", "event": "OPEN", "guard": [">", "@entity.n", 1.0], "awaitRender": [await_render()] },
                    { "from": "open", "to": "shut", "event": "INIT" }
                ]
            })
        );
    }

    #[test]
    fn a_trait_surface_carries_the_trait_id_and_the_declared_emits_with_their_event_ids() {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1,
            "orbitals": [{
                "name": "Bell",
                "entity": { "name": "Ring", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Ringer",
                    "id": "trait-ringer",
                    "emits": [{ "event": "RUNG", "eventId": "ev-rung" }, { "event": "MUTED" }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "GO" }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "GO" }]
                    }
                }]
            }]
        }))
        .unwrap();
        kernel.register_schema(schema).unwrap();
        let surface = ask(&kernel, json!({ "kind": "traitSurface", "trait": "Ringer" }));
        assert_eq!(surface["traitId"], "trait-ringer");
        assert_eq!(
            surface["emits"],
            json!([{ "event": "RUNG", "eventId": "ev-rung" }, { "event": "MUTED" }])
        );
    }

    #[test]
    fn an_unshared_browser_trait_keys_its_frame_by_its_own_name() {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1,
            "orbitals": [{
                "name": "Shelf",
                "entity": { "name": "Book", "collection": "books", "browser": true, "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Reader",
                    "linkedEntity": "Book",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "READ" }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "READ" }]
                    }
                }]
            }]
        }))
        .unwrap();
        kernel.register_schema(schema).unwrap();
        let surface = ask(
            &kernel,
            json!({ "kind": "traitSurface", "trait": "Reader" }),
        );
        assert_eq!(surface["frameKey"], "Reader");
        assert_eq!(surface["browser"], true);
        assert_eq!(surface["shared"], false);
        assert_eq!(surface["ticks"], json!([]));
        assert!(surface.get("orbitalId").is_none(), "{surface}");
    }

    #[test]
    fn a_program_surface_holds_every_trait_and_the_browser_entities() {
        let kernel = surface_kernel();
        let shelf: OrbitalSchema = serde_json::from_value(json!({
            "name": "Shelves",
            "typeContractVersion": 1,
            "orbitals": [{
                "name": "Shelf",
                "entity": { "name": "Book", "collection": "books", "browser": true, "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Reader",
                    "linkedEntity": "Book",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "READ" }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "READ" }]
                    }
                }]
            }]
        }))
        .unwrap();
        kernel.register_schema(shelf).unwrap();
        let program = ask(&kernel, json!({ "kind": "programSurface" }));
        assert_eq!(program["browserEntities"], json!(["Book"]));
        let traits = program["traits"].as_array().expect("traits");
        let names: Vec<_> = traits.iter().map(|t| t["orbital"].clone()).collect();
        assert_eq!(names, vec![json!("Door"), json!("Shelf")]);
        assert_eq!(
            traits[0],
            ask(&kernel, json!({ "kind": "traitSurface", "trait": "Gate" }))
        );
        assert_eq!(
            traits[1],
            ask(
                &kernel,
                json!({ "kind": "traitSurface", "trait": "Reader" })
            )
        );
    }

    #[test]
    fn a_transition_without_await_render_round_trips_unchanged() {
        let transition = json!({ "from": "a", "to": "b", "event": "GO" });
        let parsed: orbital_core_base::schema::TransitionDefinition =
            serde_json::from_value(transition.clone()).expect("transition");
        assert_eq!(parsed.await_render, None);
        assert_eq!(serde_json::to_value(&parsed).expect("json"), transition);
    }

    #[test]
    fn an_unknown_trait_has_no_surface() {
        let k = surface_kernel();
        let err = k
            .contract_query(
                serde_json::from_value(json!({ "kind": "traitSurface", "trait": "Nope" }))
                    .expect("query"),
            )
            .expect_err("unknown trait");
        assert!(err.contains("unknown trait: Nope"), "{err}");
    }

    #[test]
    fn external_inputs_and_readable_entities_are_the_programs() {
        let k = kernel();
        let inputs = ask(&k, json!({ "kind": "externalInputs" }));
        assert_eq!(inputs[0]["event"], "ADD");
        assert_eq!(
            ask(&k, json!({ "kind": "readableEntities" })),
            json!(["Note"])
        );
        assert_eq!(
            ask(
                &k,
                json!({ "kind": "findExternalInput", "orbital": "Cal", "trait": "Push", "event": "ADD" })
            )["event"],
            "ADD"
        );
        assert!(ask(&k, json!({ "kind": "findExternalInput", "orbital": "Cal", "trait": "Push", "event": "PUSH" })).is_null());
    }

    #[test]
    fn a_host_with_no_program_gets_the_kernels_empty_answers() {
        assert!(contract_query_without_program(
            serde_json::from_value(json!({ "kind": "findExternalInput", "orbital": "Cal", "trait": "Push", "event": "ADD" })).expect("query")
        )
        .expect("answer")
        .is_null());
        assert_eq!(
            contract_query_without_program(
                serde_json::from_value(json!({ "kind": "externalInputs" })).expect("query")
            )
            .expect("answer"),
            json!([])
        );
        let hook = contract_query_without_program(
            serde_json::from_value(json!({ "kind": "findInbound", "name": "hook" }))
                .expect("query"),
        )
        .expect("answer");
        assert_eq!(hook["answer"]["status"], 404);
        assert_eq!(
            hook,
            ask(
                &RuntimeKernel::new(
                    RuntimeEnvironment::Server,
                    Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
                    KernelConfig::default()
                ),
                json!({ "kind": "findInbound", "name": "hook" })
            )
        );
    }

    #[test]
    fn payload_and_address_asks_are_the_kernels() {
        let k = kernel();
        let caller = json!({ "orbital": "Cal", "trait": "Push" });
        assert_eq!(
            ask(
                &k,
                json!({ "kind": "findInputByAddress", "address": "ADD", "caller": caller })
            )["event"],
            "ADD"
        );
        assert!(ask(
            &k,
            json!({ "kind": "findInputByAddress", "address": "PUSH", "caller": caller })
        )
        .is_null());
        assert!(ask(
            &k,
            json!({ "kind": "findInputByAddress", "address": "*.ADD", "caller": caller })
        )
        .is_null());
        let fields = json!([{ "name": "title", "type": "string", "required": true }]);
        assert_eq!(
            ask(
                &k,
                json!({ "kind": "payloadSchemaToJsonSchema", "fields": fields })
            ),
            json!({ "type": "object", "additionalProperties": false, "properties": { "title": { "type": "string" } }, "required": ["title"] })
        );
        assert_eq!(
            ask(
                &k,
                json!({ "kind": "payloadIssues", "fields": fields, "value": {} })
            ),
            json!([{ "path": "title", "reason": "missing" }])
        );
        assert_eq!(
            ask(
                &k,
                json!({ "kind": "payloadIssues", "fields": fields, "value": { "title": "x" } })
            ),
            json!([])
        );
    }

    #[test]
    fn an_inbound_hook_resolves_to_its_route_and_an_unserved_name_to_the_404_answer() {
        let k = kernel();
        let found = ask(&k, json!({ "kind": "findInbound", "name": "hook" }));
        assert_eq!(found["route"]["event"], "PUSH");
        assert_eq!(
            ask(&k, json!({ "kind": "findInbound", "name": "other" }))["answer"]["status"],
            404
        );
        let only = ask(
            &k,
            json!({ "kind": "inboundOnly", "orbital": "Cal", "event": "PUSH" }),
        );
        assert_eq!(only["trait"], "Push");
        assert!(only["message"]
            .as_str()
            .unwrap()
            .contains("/api/hooks/<name>"));
        assert!(ask(
            &k,
            json!({ "kind": "inboundOnly", "orbital": "Cal", "event": "ADD" })
        )
        .is_null());
    }

    #[test]
    fn an_inbound_payload_and_answer_are_the_kernels() {
        let k = kernel();
        let route = ask(&k, json!({ "kind": "findInbound", "name": "hook" }))["route"].clone();
        let payload = ask(
            &k,
            json!({ "kind": "inboundPayload", "route": route, "headers": { "X-A": "1" }, "rawBody": "{\"a\":1}" }),
        );
        assert_eq!(payload["headers"]["x-a"], "1");
        assert_eq!(payload["body"]["a"].as_f64(), Some(1.0));
        let answered = ask(
            &k,
            json!({ "kind": "inboundAnswer", "route": route, "emitted": [{ "event": "ANSWERED", "payload": { "status": 201, "body": "made" } }] }),
        );
        assert_eq!(
            (answered["status"].as_u64(), answered["body"].as_str()),
            (Some(201), Some("made"))
        );
        let unanswered = ask(
            &k,
            json!({ "kind": "inboundAnswer", "route": route, "emitted": [], "error": "boom" }),
        );
        assert_eq!(
            (unanswered["status"].as_u64(), unanswered["body"].as_str()),
            (Some(500), Some("{\"error\":\"boom\"}"))
        );
        assert_eq!(
            ask(&k, json!({ "kind": "credentialUnset", "credential": "T" }))["status"],
            401
        );
        assert_eq!(
            ask(&k, json!({ "kind": "dispatchFailed", "message": "m" }))["status"],
            500
        );
    }

    #[test]
    fn an_entity_is_owned_by_the_orbital_that_declares_it_inline() {
        let k = kernel();
        assert_eq!(
            ask(&k, json!({ "kind": "entityOwner", "entity": "Note" })),
            json!("Cal")
        );
        assert_eq!(
            ask(&k, json!({ "kind": "entityOwner", "entity": "Scratch" })),
            json!("Cal")
        );
        assert!(ask(&k, json!({ "kind": "entityOwner", "entity": "Ghost" })).is_null());
    }

    #[test]
    fn the_identity_entity_is_named_when_the_program_declares_one() {
        let k = kernel();
        assert!(
            ask(&k, json!({ "kind": "identityEntity" })).is_null(),
            "control: no [identity] entity declared"
        );
        let with_identity = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App", "typeContractVersion": 1,
            "orbitals": [{
                "name": "People",
                "entity": { "name": "Person", "collection": "people", "identity": true, "fields": [{ "name": "id", "type": "string" }] },
                "traits": [], "pages": []
            }]
        }))
        .unwrap();
        with_identity.register_schema(schema).unwrap();
        assert_eq!(
            ask(&with_identity, json!({ "kind": "identityEntity" })),
            json!("Person")
        );
    }

    #[test]
    fn rows_are_narrowed_by_the_declared_read_policy_for_the_viewer() {
        let k = kernel();
        let rows = json!([{ "id": "1", "owner": "u1" }, { "id": "2", "owner": "u2" }]);
        let mine = ask(
            &k,
            json!({ "kind": "readRows", "entity": "Note", "rows": rows, "viewer": { "id": "u1" } }),
        );
        assert_eq!(mine, json!([{ "id": "1", "owner": "u1" }]));
        let all = ask(
            &k,
            json!({ "kind": "readRows", "entity": "Scratch", "rows": rows, "viewer": { "id": "u1" } }),
        );
        assert_eq!(
            all.as_array().unwrap().len(),
            2,
            "control: an entity with no @read keeps every row"
        );
    }
}
