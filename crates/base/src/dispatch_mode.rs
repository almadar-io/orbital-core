//! Client dispatch strategy — the ONE rule, computed from declared facts.
//!
//! `DispatchMode` decides whether a trait's transitions await a server round
//! trip, run purely client-side, or commit locally then reconcile in the
//! background. Inputs (owner ruling 2026-09-24): the trait's `local` flag,
//! whether its linked entity is `[runtime]`, and whether its own effect row
//! reaches the server — every operator's site is declared registry data
//! (`effects::runs_on`), so the mode is computed, never guessed. A `[runtime]`
//! entity lives in the client: its traits stay client-only unless their own
//! effects need the server. Both entry points (`ir::types::OirTrait` for the
//! compiled path, `schema::types::TraitDefinition` for the runtime kernel)
//! route through [`compute`] — see `dispatch_mode_ir_and_schema_agree`.

use serde::{Deserialize, Serialize};

/// Client dispatch strategy for a trait's transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DispatchMode {
    /// Server-gated: dispatch awaits the persisted round trip.
    PersistedAwaited,
    /// Client-only dispatch, no server round trip: a `local` trait, or a
    /// `[runtime]` trait whose own effects never reach the server.
    HybridClientOnly,
    /// `[runtime]` trait whose own effects reach the server: commits locally,
    /// then posts and folds the server's reply (rolled back on rejection).
    RuntimeOptimistic,
}

/// The one rule: `local` is always client-only; a `[runtime]` entity is
/// client-only unless the trait's own effects reach the server (then
/// optimistic); anything else awaits the server.
pub fn compute(local: bool, entity_is_runtime: bool, touches_server: bool) -> DispatchMode {
    if local {
        return DispatchMode::HybridClientOnly;
    }
    if entity_is_runtime {
        return if touches_server {
            DispatchMode::RuntimeOptimistic
        } else {
            DispatchMode::HybridClientOnly
        };
    }
    DispatchMode::PersistedAwaited
}

#[cfg(test)]
mod tests {
    use super::*;
    use {crate::ir::effects::OirEffect, crate::ir::types::{OirEntity, OirPersistence, OirTrait, OirTransition}, crate::ir::expr::OirExpr};
    use crate::schema::types::{EntityDefinition, TraitDefinition};

    #[test]
    fn dispatch_mode_rule_table() {
        // `local` wins everywhere.
        for (runtime, server) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(
                compute(true, runtime, server),
                DispatchMode::HybridClientOnly
            );
        }
        // A `[runtime]` trait is client-only unless its own effects reach the server.
        assert_eq!(compute(false, true, false), DispatchMode::HybridClientOnly);
        assert_eq!(compute(false, true, true), DispatchMode::RuntimeOptimistic);
        // A persisted entity always awaits the server.
        assert_eq!(compute(false, false, false), DispatchMode::PersistedAwaited);
        assert_eq!(compute(false, false, true), DispatchMode::PersistedAwaited);
    }

    fn entity(runtime: bool) -> EntityDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "E",
            "persistence": if runtime { "runtime" } else { "persistent" },
            "fields": [{ "name": "id", "type": "string" }],
        }))
        .expect("entity fixture")
    }

    fn schema_trait(effects: serde_json::Value, ticks: serde_json::Value) -> TraitDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "T",
            "ticks": ticks,
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": effects }]
            },
        }))
        .expect("trait fixture")
    }

    #[test]
    fn a_runtime_trait_with_no_server_effect_is_client_only() {
        let t = schema_trait(
            serde_json::json!([["set", "@entity.x", 1]]),
            serde_json::json!([]),
        );
        assert_eq!(
            t.dispatch_mode(Some(&entity(true)), &crate::runs_on::Residence::none()),
            DispatchMode::HybridClientOnly
        );
    }

    #[test]
    fn a_nested_server_effect_makes_a_runtime_trait_optimistic() {
        let t = schema_trait(
            serde_json::json!([["when", true, ["fetch", "Row", {}]]]),
            serde_json::json!([]),
        );
        assert_eq!(
            t.dispatch_mode(Some(&entity(true)), &crate::runs_on::Residence::none()),
            DispatchMode::RuntimeOptimistic
        );
    }

    #[test]
    fn a_client_tick_server_effect_counts_a_background_tick_does_not() {
        let tick = |bg: bool| {
            serde_json::json!([{ "name": "t", "interval": 100, "runsInBackground": bg,
            "effects": [["fetch", "Row", {}]] }])
        };
        let client = schema_trait(serde_json::json!([]), tick(false));
        let background = schema_trait(serde_json::json!([]), tick(true));
        assert_eq!(
            client.dispatch_mode(Some(&entity(true)), &crate::runs_on::Residence::none()),
            DispatchMode::RuntimeOptimistic
        );
        assert_eq!(
            background.dispatch_mode(Some(&entity(true)), &crate::runs_on::Residence::none()),
            DispatchMode::HybridClientOnly
        );
    }

    /// A branch that can never run is no server work: a ui-* wrapper's
    /// `INIT when @config.selfFetch (fetch …)` with `selfFetch: false` inlined.
    #[test]
    fn a_statically_dead_server_effect_does_not_count() {
        let guarded: TraitDefinition = serde_json::from_value(serde_json::json!({
            "name": "T",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "guard": false,
                                  "effects": [["fetch", "Row", {}]] }]
            },
        }))
        .expect("trait fixture");
        assert_eq!(
            guarded.dispatch_mode(Some(&entity(true)), &crate::runs_on::Residence::none()),
            DispatchMode::HybridClientOnly
        );
        let dead_then = schema_trait(
            serde_json::json!([["if", false, ["fetch", "Row", {}], null]]),
            serde_json::json!([]),
        );
        assert_eq!(
            dead_then.dispatch_mode(Some(&entity(true)), &crate::runs_on::Residence::none()),
            DispatchMode::HybridClientOnly
        );
        let live_else = schema_trait(
            serde_json::json!([["if", false, null, ["fetch", "Row", {}]]]),
            serde_json::json!([]),
        );
        assert_eq!(
            live_else.dispatch_mode(Some(&entity(true)), &crate::runs_on::Residence::none()),
            DispatchMode::RuntimeOptimistic
        );
    }

    /// A binding position names a value; it never calls the operator it spells.
    #[test]
    fn a_binding_named_like_a_server_operator_is_not_server_work() {
        let optimistic = |effects: serde_json::Value| {
            schema_trait(effects, serde_json::json!([]))
                .dispatch_mode(Some(&entity(true)), &crate::runs_on::Residence::none())
                == DispatchMode::RuntimeOptimistic
        };
        assert!(!optimistic(serde_json::json!([[
            "let",
            [["swap", [">", "@a", "@b"]]],
            ["set", "@entity.x", "@swap"]
        ]])));
        assert!(!optimistic(serde_json::json!([[
            "let",
            [["fetch", 1], ["persist", 2]],
            ["set", "@entity.x", "@fetch"]
        ]])));
        assert!(!optimistic(serde_json::json!([[
            "set",
            "@entity.xs",
            ["array/map", "@entity.xs", ["fn", ["swap", "i"], "@swap"]]
        ]])));
        assert!(optimistic(serde_json::json!([[
            "let",
            [["rows", ["fetch", "Row", {}]]],
            ["set", "@entity.x", "@rows"]
        ]])));
        assert!(optimistic(serde_json::json!([[
            "let",
            [["swap", true]],
            ["persist", "create", "Row", {}]
        ]])));
        assert!(optimistic(serde_json::json!([[
            "set",
            "@entity.xs",
            [
                "array/map",
                "@entity.xs",
                ["fn", ["swap", "i"], ["fetch", "Row", {}]]
            ]
        ]])));
    }

    #[test]
    fn control_a_persisted_trait_awaits_whatever_its_effects() {
        let t = schema_trait(
            serde_json::json!([["set", "@entity.x", 1]]),
            serde_json::json!([]),
        );
        assert_eq!(
            t.dispatch_mode(Some(&entity(false)), &crate::runs_on::Residence::none()),
            DispatchMode::PersistedAwaited
        );
    }

    fn oir_fetch() -> OirExpr {
        OirExpr::Effect {
            effect: Box::new(OirEffect::Fetch {
                entity_type: "Row".to_string(),
                id: None,
                filter: None,
                order_by: None,
                offset: None,
                limit: None,
                include: None,
                nearest: None,
                emit: None,
            }),
        }
    }

    /// Both entry points — `OirTrait::dispatch_mode` (compiled path) and
    /// `TraitDefinition::dispatch_mode` (runtime kernel) — must agree on the
    /// full (local × persistence × server effect) matrix.
    #[test]
    fn dispatch_mode_ir_and_schema_agree() {
        for local in [true, false] {
            for entity_is_runtime in [true, false] {
                for (server, dead) in [(true, false), (false, false), (true, true)] {
                    let mut oir_trait = OirTrait::new("T");
                    oir_trait.local = local;
                    if server {
                        let mut tr = OirTransition::new("idle", "idle", "GO");
                        tr.effects = vec![OirExpr::Conditional {
                            condition: Box::new(OirExpr::bool(!dead)),
                            then_branch: Box::new(oir_fetch()),
                            else_branch: Box::new(OirExpr::null()),
                        }];
                        oir_trait.transitions.push(tr);
                    }
                    let mut oir_entity = OirEntity::new("E");
                    oir_entity.persistence = if entity_is_runtime {
                        OirPersistence::Runtime
                    } else {
                        OirPersistence::Persistent
                    };
                    let ir_mode = oir_trait
                        .dispatch_mode(Some(&oir_entity), &crate::runs_on::Residence::none());

                    let mut schema = schema_trait(
                        if server {
                            serde_json::json!([["if", !dead, ["fetch", "Row", {}], null]])
                        } else {
                            serde_json::json!([])
                        },
                        serde_json::json!([]),
                    );
                    schema.local = local;
                    let schema_mode = schema.dispatch_mode(
                        Some(&entity(entity_is_runtime)),
                        &crate::runs_on::Residence::none(),
                    );

                    assert_eq!(
                        ir_mode, schema_mode,
                        "local={local} runtime={entity_is_runtime} server={server} dead={dead}"
                    );
                    assert_eq!(ir_mode, compute(local, entity_is_runtime, server && !dead));
                }
            }
        }
    }
}
