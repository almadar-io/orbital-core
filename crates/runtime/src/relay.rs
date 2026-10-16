//! Which events a client relays to the server (`relayMask`, `clientRelays`).
//!
//! The JS twin is `evaluateOrbitalEvent.ts` (mask derivation, `relayMaskKey`,
//! the masking test in `collectListenerTargets`). A *relay mask* names the
//! traits (or `(trait, event)` pairs) the requesting client completes itself;
//! the listens fan-out skips them.

use std::collections::BTreeSet;

/// Strip the `UI:` bus prefix from an event key (`normalizeEventKey`).
pub fn normalize_event_key(event: &str) -> &str {
    event.strip_prefix("UI:").unwrap_or(event)
}

/// The traits and `(trait, event)` pairs a fan-out skips.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayMask {
    traits: BTreeSet<String>,
    pairs: BTreeSet<(String, String)>,
}

impl RelayMask {
    /// Masks nothing (an explicit empty mask: the host runs every listener).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Masks every event of each named trait.
    pub fn of_traits<I, S>(traits: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            traits: traits.into_iter().map(Into::into).collect(),
            pairs: BTreeSet::new(),
        }
    }

    /// Masks one `(trait, event)` pair; the event is normalized.
    pub fn of_pairs<I, T, E>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (T, E)>,
        T: Into<String>,
        E: AsRef<str>,
    {
        Self {
            traits: BTreeSet::new(),
            pairs: pairs
                .into_iter()
                .map(|(t, e)| (t.into(), normalize_event_key(e.as_ref()).to_string()))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.traits.is_empty() && self.pairs.is_empty()
    }

    /// True when a listener `trait_name` whose `listens` entry dispatches
    /// `triggers` is skipped by this mask.
    pub fn masks(&self, trait_name: &str, triggers: &str) -> bool {
        self.traits.contains(trait_name)
            || self.pairs.contains(&(
                trait_name.to_string(),
                normalize_event_key(triggers).to_string(),
            ))
    }
}

/// The listener-side test of `collectListenerTargets`: `mask` is `None` when
/// the dispatch masks nothing.
pub fn listener_masked(mask: Option<&RelayMask>, trait_name: &str, triggers: &str) -> bool {
    mask.is_some_and(|m| m.masks(trait_name, triggers))
}

/// What the request itself says about its shape.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestFacts<'a> {
    pub has_target_trait: bool,
    pub has_mount: bool,
    /// `request.traits`: the client's declared dispatch set.
    pub declared_traits: Option<&'a [String]>,
    pub has_entity_by_trait: bool,
}

impl RequestFacts<'_> {
    /// A server leg the client delegated: it folds the response, relaying nothing.
    pub fn is_delegated_server_leg(&self) -> bool {
        (self.has_target_trait || self.has_mount)
            && (self.declared_traits.is_some() || self.has_entity_by_trait)
    }
}

/// Derive the relay mask for one dispatch. `explicit` always wins; otherwise
/// `client_relays` masks every trait, a delegated leg masks nothing (`None`),
/// a declared dispatch masks its declared set, a single-target dispatch masks
/// only `(trait, event)` of each target, and discovery masks the resolved
/// targets.
pub fn derive_relay_mask(
    explicit: Option<&RelayMask>,
    client_relays: bool,
    all_traits: &[String],
    facts: &RequestFacts<'_>,
    targets: &[String],
    event: &str,
) -> Option<RelayMask> {
    if let Some(mask) = explicit {
        return Some(mask.clone());
    }
    if client_relays {
        return Some(RelayMask::of_traits(all_traits.iter().cloned()));
    }
    if facts.is_delegated_server_leg() {
        return None;
    }
    if let Some(declared) = facts.declared_traits {
        return Some(RelayMask::of_traits(declared.iter().cloned()));
    }
    if facts.has_target_trait {
        return Some(RelayMask::of_pairs(
            targets.iter().map(|t| (t.clone(), event)),
        ));
    }
    Some(RelayMask::of_traits(targets.iter().cloned()))
}

/// Whether a trait's own emit cascades back into the same trait: false when
/// the client relays every emit itself (the compiled wire).
pub fn self_cascade(client_relays: bool) -> bool {
    !client_relays
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    const ALL: [&str; 3] = ["Src", "Echo", "Other"];

    // relay-mask-request-shapes.test.ts: Echo and Other listen to Src.PING (triggers HEAR).
    fn delivered(mask: Option<&RelayMask>) -> Vec<&'static str> {
        ["Echo", "Other"]
            .into_iter()
            .filter(|t| !listener_masked(mask, t, "HEAR"))
            .collect()
    }

    #[test]
    fn declared_dispatch_masks_exactly_its_declared_traits() {
        let declared = names(&["Src", "Echo"]);
        let facts = RequestFacts {
            declared_traits: Some(&declared),
            ..Default::default()
        };
        let m = derive_relay_mask(None, false, &names(&ALL), &facts, &declared, "GO");
        assert_eq!(delivered(m.as_ref()), vec!["Other"]);
    }

    #[test]
    fn control_trait_left_out_of_declared_set_is_delivered() {
        let declared = names(&["Src"]);
        let facts = RequestFacts {
            declared_traits: Some(&declared),
            ..Default::default()
        };
        let m = derive_relay_mask(None, false, &names(&ALL), &facts, &declared, "GO");
        assert_eq!(delivered(m.as_ref()), vec!["Echo", "Other"]);
    }

    #[test]
    fn discovery_masks_the_targets_it_resolved() {
        let targets = names(&["Src", "Echo"]);
        let m = derive_relay_mask(
            None,
            false,
            &names(&ALL),
            &RequestFacts::default(),
            &targets,
            "GO",
        );
        assert_eq!(delivered(m.as_ref()), vec!["Other"]);
    }

    #[test]
    fn delegated_leg_masks_nothing_even_a_trait_it_carries() {
        let declared = names(&["Src", "Echo"]);
        let facts = RequestFacts {
            has_target_trait: true,
            declared_traits: Some(&declared),
            ..Default::default()
        };
        assert!(facts.is_delegated_server_leg());
        let m = derive_relay_mask(None, false, &names(&ALL), &facts, &declared, "GO");
        assert_eq!(m, None);
        assert_eq!(delivered(m.as_ref()), vec!["Echo", "Other"]);
    }

    #[test]
    fn delegated_leg_by_mount_with_entity_by_trait() {
        let facts = RequestFacts {
            has_mount: true,
            has_entity_by_trait: true,
            ..Default::default()
        };
        assert!(facts.is_delegated_server_leg());
    }

    #[test]
    fn target_trait_alone_or_traits_alone_is_not_delegated() {
        assert!(!RequestFacts {
            has_target_trait: true,
            ..Default::default()
        }
        .is_delegated_server_leg());
        let d = names(&["A"]);
        assert!(!RequestFacts {
            declared_traits: Some(&d),
            ..Default::default()
        }
        .is_delegated_server_leg());
    }

    #[test]
    fn single_target_masks_only_the_trait_event_pair() {
        let facts = RequestFacts {
            has_target_trait: true,
            ..Default::default()
        };
        let m = derive_relay_mask(
            None,
            false,
            &names(&ALL),
            &facts,
            &names(&["Echo"]),
            "UI:GO",
        )
        .unwrap();
        assert!(m.masks("Echo", "GO"));
        assert!(m.masks("Echo", "UI:GO"));
        // control: a different event sent back to the same trait still reaches it
        assert!(!m.masks("Echo", "REPLY"));
        assert!(!m.masks("Other", "GO"));
    }

    #[test]
    fn explicit_empty_mask_wins_over_every_shape() {
        let declared = names(&["Src", "Echo"]);
        let facts = RequestFacts {
            declared_traits: Some(&declared),
            ..Default::default()
        };
        let explicit = RelayMask::empty();
        let m = derive_relay_mask(
            Some(&explicit),
            false,
            &names(&ALL),
            &facts,
            &declared,
            "GO",
        );
        assert_eq!(m, Some(RelayMask::empty()));
        assert_eq!(delivered(m.as_ref()), vec!["Echo", "Other"]);
    }

    #[test]
    fn explicit_mask_wins_over_client_relays() {
        let explicit = RelayMask::of_traits(["Echo"]);
        let m = derive_relay_mask(
            Some(&explicit),
            true,
            &names(&ALL),
            &RequestFacts::default(),
            &[],
            "GO",
        );
        assert_eq!(m, Some(explicit));
    }

    #[test]
    fn client_relays_masks_every_trait_and_disables_self_cascade() {
        let m = derive_relay_mask(
            None,
            true,
            &names(&ALL),
            &RequestFacts::default(),
            &[],
            "GO",
        );
        assert!(delivered(m.as_ref()).is_empty());
        assert!(m.unwrap().masks("Src", "ANY"));
        assert!(!self_cascade(true));
        assert!(self_cascade(false));
    }

    #[test]
    fn client_relays_beats_a_delegated_leg() {
        let facts = RequestFacts {
            has_target_trait: true,
            has_entity_by_trait: true,
            ..Default::default()
        };
        let m = derive_relay_mask(None, true, &names(&ALL), &facts, &[], "GO");
        assert!(m.is_some());
    }

    #[test]
    fn no_mask_masks_nothing() {
        assert!(!listener_masked(None, "Echo", "HEAR"));
        assert!(RelayMask::empty().is_empty());
    }
}
