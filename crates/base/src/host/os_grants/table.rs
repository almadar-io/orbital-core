//! The grants a kernel holds per orbital: each orbital's effective set (the app level its program declares plus its own)
//! and the app level alone, which answers an ask that names no orbital.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::{EntryProblem, OsAnchors, OsGrants, OsRequirement};
use crate::schema::OrbitalSchema;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsGrantTable {
    app: OsGrants,
    orbitals: BTreeMap<String, OsGrants>,
}

impl From<OsGrants> for OsGrantTable {
    /// One set for every ask: what a host holds when it has no per-orbital declarations.
    fn from(app: OsGrants) -> Self {
        Self {
            app,
            orbitals: BTreeMap::new(),
        }
    }
}

impl OsGrantTable {
    /// Adds a program: its app level to the app set, and each orbital's effective grants to that orbital's set.
    pub fn register(&mut self, schema: &OrbitalSchema) {
        if let Some(app) = &schema.os_grants {
            self.app.union(app);
        }
        for orbital in &schema.orbitals {
            self.orbitals
                .entry(orbital.name.clone())
                .or_default()
                .union(&schema.orbital_os_grants(orbital));
        }
    }

    /// The grants that answer an ask from `orbital`: its own effective set; the app level for an ask that names no
    /// orbital or one no program registered.
    pub fn for_orbital(&self, orbital: Option<&str>) -> &OsGrants {
        orbital
            .and_then(|name| self.orbitals.get(name))
            .unwrap_or(&self.app)
    }

    /// The one check every host makes: whether `orbital`'s grants cover `need`.
    pub fn allows(&self, orbital: Option<&str>, need: &OsRequirement) -> bool {
        self.for_orbital(orbital).allows(need)
    }

    /// Every grant any orbital or the app level holds, unioned: for reports and device claims, never to answer an ask.
    pub fn declared(&self) -> OsGrants {
        let mut all = self.app.clone();
        for grants in self.orbitals.values() {
            all.union(grants);
        }
        all
    }

    pub fn is_empty(&self) -> bool {
        self.app.is_empty() && self.orbitals.values().all(OsGrants::is_empty)
    }

    /// Every set with its anchored entries placed under `anchors`; the entries that grant nothing, once each.
    pub fn resolve_anchors(&self, anchors: &OsAnchors) -> (Self, Vec<EntryProblem>) {
        let (app, mut problems) = self.app.resolve_anchors(anchors);
        let mut orbitals = BTreeMap::new();
        for (name, grants) in &self.orbitals {
            let (resolved, found) = grants.resolve_anchors(anchors);
            for problem in found {
                if !problems
                    .iter()
                    .any(|seen| seen.path == problem.path && seen.message == problem.message)
                {
                    problems.push(problem);
                }
            }
            orbitals.insert(name.clone(), resolved);
        }
        (Self { app, orbitals }, problems)
    }
}
