//! Client dispatch strategy: read from the trait's declared `dispatch`.
//!
//! `DispatchMode` decides whether a trait's transitions await a server round
//! trip, run purely client-side, or commit locally then reconcile in the
//! background. The only input is the trait's declared mode (`[client]`,
//! `[optimistic]`, or nothing = server); nothing is inferred from the linked
//! entity or the trait's effects. Both entry points (`ir::types::OirTrait` for
//! the compiled path, `schema::types::TraitDefinition` for the runtime kernel)
//! route through [`compute`] — see `dispatch_mode_ir_and_schema_agree`.

use serde::{Deserialize, Serialize};

use crate::schema::types::DispatchMode as Declared;

/// Client dispatch strategy for a trait's transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DispatchMode {
    /// Server-gated: dispatch awaits the persisted round trip.
    PersistedAwaited,
    /// Client-only dispatch, no server round trip: a `[client]` trait.
    HybridClientOnly,
    /// An `[optimistic]` trait: commits locally, then posts and folds the
    /// server's reply (rolled back on rejection).
    RuntimeOptimistic,
}

/// The one rule: `client` is client-only, `optimistic` is optimistic, anything
/// undeclared awaits the server.
pub fn compute(declared: Option<Declared>) -> DispatchMode {
    match declared {
        Some(Declared::Client) => DispatchMode::HybridClientOnly,
        Some(Declared::Optimistic) => DispatchMode::RuntimeOptimistic,
        Some(Declared::Server) | None => DispatchMode::PersistedAwaited,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use {crate::ir::types::{OirEntity, OirPersistence, OirTrait}};
    use crate::runs_on::Residence;
    use crate::schema::types::{EntityDefinition, TraitDefinition};

    #[test]
    fn dispatch_mode_rule_table() {
        assert_eq!(compute(Some(Declared::Client)), DispatchMode::HybridClientOnly);
        assert_eq!(compute(Some(Declared::Optimistic)), DispatchMode::RuntimeOptimistic);
        assert_eq!(compute(Some(Declared::Server)), DispatchMode::PersistedAwaited);
        assert_eq!(compute(None), DispatchMode::PersistedAwaited);
    }

    #[test]
    fn server_is_server_whatever_the_entity_or_effects() {
        for effects in [serde_json::json!([["set", "@entity.x", 1]]), serde_json::json!([["fetch", "Row", {}]])] {
            let t = schema_trait(Some("server"), effects);
            assert_eq!(mode_of(&t, true), DispatchMode::PersistedAwaited);
            assert_eq!(mode_of(&t, false), DispatchMode::PersistedAwaited);
        }
    }

    fn entity(runtime: bool) -> EntityDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "E",
            "persistence": if runtime { "runtime" } else { "persistent" },
            "fields": [{ "name": "id", "type": "string" }],
        }))
        .expect("entity fixture")
    }

    fn schema_trait(dispatch: Option<&str>, effects: serde_json::Value) -> TraitDefinition {
        let mut value = serde_json::json!({
            "name": "T",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": effects }]
            },
        });
        if let Some(mode) = dispatch {
            value["dispatch"] = serde_json::json!(mode);
        }
        serde_json::from_value(value).expect("trait fixture")
    }

    fn mode_of(t: &TraitDefinition, runtime: bool) -> DispatchMode {
        t.dispatch_mode(Some(&entity(runtime)), &Residence::none())
    }

    #[test]
    fn a_trait_with_no_mode_is_server_even_on_a_runtime_entity() {
        let t = schema_trait(None, serde_json::json!([["set", "@entity.x", 1]]));
        assert_eq!(mode_of(&t, true), DispatchMode::PersistedAwaited);
        assert_eq!(mode_of(&t, false), DispatchMode::PersistedAwaited);
    }

    #[test]
    fn a_server_effect_on_a_runtime_entity_does_not_make_an_undeclared_trait_optimistic() {
        let t = schema_trait(None, serde_json::json!([["fetch", "Row", {}]]));
        assert_eq!(mode_of(&t, true), DispatchMode::PersistedAwaited);
    }

    #[test]
    fn client_is_client_only_whatever_the_entity_or_effects() {
        for effects in [serde_json::json!([["set", "@entity.x", 1]]), serde_json::json!([["fetch", "Row", {}]])] {
            let t = schema_trait(Some("client"), effects);
            assert_eq!(mode_of(&t, true), DispatchMode::HybridClientOnly);
            assert_eq!(mode_of(&t, false), DispatchMode::HybridClientOnly);
        }
    }

    #[test]
    fn optimistic_is_optimistic_whatever_the_entity_or_effects() {
        for effects in [serde_json::json!([["set", "@entity.x", 1]]), serde_json::json!([["fetch", "Row", {}]])] {
            let t = schema_trait(Some("optimistic"), effects);
            assert_eq!(mode_of(&t, true), DispatchMode::RuntimeOptimistic);
            assert_eq!(mode_of(&t, false), DispatchMode::RuntimeOptimistic);
        }
    }

    #[test]
    fn an_unknown_declared_mode_is_refused_at_deserialization() {
        let bad = serde_json::json!({
            "name": "T", "dispatch": "local",
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] },
        });
        assert!(serde_json::from_value::<TraitDefinition>(bad).is_err());
    }

    #[test]
    fn the_retired_local_flag_is_not_a_dispatch_declaration() {
        let t: TraitDefinition = serde_json::from_value(serde_json::json!({
            "name": "T", "local": true,
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] },
        }))
        .expect("trait fixture");
        assert_eq!(mode_of(&t, true), DispatchMode::PersistedAwaited);
    }

    #[test]
    fn dispatch_mode_ir_and_schema_agree() {
        for dispatch in [None, Some(Declared::Client), Some(Declared::Optimistic), Some(Declared::Server)] {
            for entity_is_runtime in [true, false] {
                let mut oir_trait = OirTrait::new("T");
                oir_trait.dispatch = dispatch;
                let mut oir_entity = OirEntity::new("E");
                oir_entity.persistence = if entity_is_runtime {
                    OirPersistence::Runtime
                } else {
                    OirPersistence::Persistent
                };
                let ir_mode = oir_trait.dispatch_mode(Some(&oir_entity), &Residence::none());

                let mut schema = schema_trait(None, serde_json::json!([]));
                schema.dispatch = dispatch;
                let schema_mode = mode_of(&schema, entity_is_runtime);

                assert_eq!(ir_mode, schema_mode, "dispatch={dispatch:?} runtime={entity_is_runtime}");
                assert_eq!(ir_mode, compute(dispatch));
            }
        }
    }
}
