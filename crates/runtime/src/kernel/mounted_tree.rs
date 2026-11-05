//! `program/self { mounted: true }`: the mounted orbital as a tree (EXEC-DIST-15). The kernel the step runs on answers
//! it from the page its session's last route mount opened: that kernel's `boot` endpoint traits first, then the page's
//! own traits that no mounted trait embeds, each followed by the traits it embeds (`@trait.X`), depth first in the order
//! first met. A trait embedded in several places sits at the first. `observer` marks the asking trait and every trait that
//! listens to an observer, transitively: a view of the page that traced them would trace its own work.

use super::callsite_capture::{embedding_states, renders};
use super::*;
use crate::trait_owners::TraitOwners;
use orbital_core_base::schema::types::{ListenSource, StringOrArray, TraitDefinition};

/// The session key of the program session (ticks, boot, hooks).
const PROGRAM_SESSION: &str = "";

fn names(list: impl IntoIterator<Item = String>) -> Value {
    Value::array(list.into_iter().map(Value::string).collect::<Vec<_>>())
}

fn opt_string(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::string)
}

/// The events an effect list emits (`["emit", EVENT, ...]`), in the order first met.
fn emitted_events(value: &serde_json::Value, into: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            if let (Some("emit"), Some(event)) = (
                items.first().and_then(serde_json::Value::as_str),
                items.get(1).and_then(serde_json::Value::as_str),
            ) {
                if !into.iter().any(|e| e == event) {
                    into.push(event.to_string());
                }
            }
            items.iter().for_each(|v| emitted_events(v, into));
        }
        serde_json::Value::Object(map) => map.values().for_each(|v| emitted_events(v, into)),
        _ => {}
    }
}

fn machine_entry(def: &TraitDefinition) -> (Value, Value) {
    let states = def
        .state_machine
        .states
        .iter()
        .map(|s| {
            let mut out = ValueMap::default();
            out.insert("name".into(), Value::string(s.name.clone()));
            out.insert("isInitial".into(), Value::Bool(s.is_initial == Some(true)));
            out.insert(
                "isTerminal".into(),
                Value::Bool(s.is_terminal == Some(true)),
            );
            Value::object(out)
        })
        .collect::<Vec<_>>();
    let transitions = def
        .state_machine
        .transitions
        .iter()
        .enumerate()
        .map(|(index, t)| {
            let from = match &t.from {
                StringOrArray::Single(s) => vec![s.clone()],
                StringOrArray::Multiple(list) => list.clone(),
            };
            let mut out = ValueMap::default();
            out.insert("index".into(), Value::from(index as i64));
            out.insert("from".into(), names(from));
            out.insert("to".into(), Value::string(t.to.clone()));
            out.insert("event".into(), Value::string(t.event.clone()));
            out.insert("guarded".into(), Value::Bool(t.guard.is_some()));
            let mut emitted: Vec<String> = Vec::new();
            emitted_events(
                &serde_json::to_value(&t.effects).unwrap_or(serde_json::Value::Null),
                &mut emitted,
            );
            out.insert("emits".into(), names(emitted));
            Value::object(out)
        })
        .collect::<Vec<_>>();
    (Value::array(states), Value::array(transitions))
}

impl RuntimeKernel {
    fn page_session_key(&self) -> Arc<str> {
        self.thread_session()
            .unwrap_or_else(|| Arc::from(PROGRAM_SESSION))
    }

    /// This thread's session opened `page` by a route mount.
    pub(super) fn record_session_page(&self, page: &str) {
        let session = self.page_session_key().to_string();
        self.sessions.pages.write().insert(session, Arc::from(page));
    }

    /// This thread's session opened `route` by a route mount: record the page it resolves to. A browser kernel calls it
    /// for the route mounts it runs itself, whose `$MOUNT` its host delivers (EXEC-SEED-7).
    pub fn record_route_page(&self, route: &str) -> Result<(), String> {
        if let Some(page) = self.page_circuit(None, Some(route))?.page {
            self.record_session_page(&page);
        }
        Ok(())
    }

    /// The page this thread's session last opened by a route mount.
    pub fn session_page(&self) -> Option<Arc<str>> {
        self.sessions
            .pages
            .read()
            .get(&*self.page_session_key())
            .cloned()
    }

    fn boot_traits(&self) -> Vec<String> {
        use orbital_core_base::protocols::{protocol_after_preflight, ProtocolSide};
        let mut out: Vec<String> = Vec::new();
        self.each_inline_page(|page| {
            let boot = protocol_after_preflight(page.protocol_name())
                .is_some_and(|spec| spec.side == ProtocolSide::Start);
            if !boot {
                return;
            }
            for name in page.traits.iter().flatten().filter_map(|t| t.trait_name()) {
                if !out.iter().any(|n| n == name) {
                    out.push(name.to_string());
                }
            }
        });
        out
    }

    /// Each running instance of `trait_name` in this thread's session: `{ entityId, state }`.
    fn session_instances(&self, trait_name: &str) -> Value {
        let held: Vec<(Option<String>, String)> = self
            .state_manager
            .read()
            .get_instances_for_trait(trait_name)
            .into_iter()
            .filter_map(|s| {
                Some((
                    self.session_instance_id(s.entity_id.as_deref())?,
                    s.current_state.clone(),
                ))
            })
            .collect();
        Value::array(
            held.into_iter()
                .map(|(entity_id, state)| {
                    let mut row = ValueMap::default();
                    row.insert(
                        "entityId".into(),
                        Value::string(entity_id.map_or_else(
                            || "singleton".to_string(),
                            |id| crate::process_result::public_entity_id(&id).to_string(),
                        )),
                    );
                    row.insert("state".into(), Value::string(state));
                    Value::object(row)
                })
                .collect::<Vec<_>>(),
        )
    }

    /// The mounted orbital of this thread's session as `{ page, traits }`, each trait
    /// `{ trait, orbital, parent, depth, embeddedIn, renders, observer, stepsIn, linkedEntity, instances, states, transitions, emits, listens }`.
    pub(super) fn mounted_tree(&self) -> Result<Value, String> {
        let page = self.session_page().ok_or(
            "program/self mounted: this session has opened no page; a route mount opens one",
        )?;
        let circuit = self.page_circuit(Some(&page), None)?;
        let boot = self.boot_traits();
        let boot_embeds = self.trait_refs_from(&boot);
        let mut mounted: Vec<String> = Vec::new();
        for name in boot.iter().chain(&boot_embeds).chain(&circuit.traits) {
            if !mounted.contains(name) {
                mounted.push(name.clone());
            }
        }
        let defs = self.registry.trait_definitions.read();
        let refs = self.registry.embedded_trait_refs.read();
        let children = |name: &str| -> Vec<String> {
            refs.get(name)
                .map(|list| {
                    list.iter()
                        .filter(|c| mounted.contains(c))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        let embedded: Vec<String> = mounted.iter().flat_map(|n| children(n)).collect();
        let roots: Vec<String> = mounted
            .iter()
            .filter(|n| !embedded.contains(n))
            .cloned()
            .collect();
        let (_, asker) = self.caller_position();
        let sources = |def: &TraitDefinition| -> Vec<String> {
            def.listens
                .iter()
                .flat_map(|l| l.iter())
                .filter_map(|l| match &l.source {
                    Some(ListenSource::Trait { trait_name, .. })
                    | Some(ListenSource::Orbital { trait_name, .. }) => Some(trait_name.clone()),
                    _ => None,
                })
                .collect()
        };
        let mut observers: Vec<String> = vec![asker.clone()];
        loop {
            let reached: Vec<String> = mounted
                .iter()
                .filter(|t| !observers.contains(t))
                .filter(|t| {
                    defs.get(t.as_str())
                        .is_some_and(|def| sources(def).iter().any(|s| observers.contains(s)))
                })
                .cloned()
                .collect();
            if reached.is_empty() {
                break;
            }
            observers.extend(reached);
        }
        let owners = TraitOwners::from_kernel(self);
        let mut placed: Vec<String> = Vec::new();
        let mut out: Vec<Value> = Vec::new();
        let mut stack: Vec<(String, Option<String>, u64)> =
            roots.into_iter().rev().map(|r| (r, None, 0)).collect();
        while let Some((name, parent, depth)) = stack.pop() {
            if placed.contains(&name) {
                continue;
            }
            placed.push(name.clone());
            let def = defs.get(&name).ok_or_else(|| {
                format!("program/self mounted: `{name}` is not a registered trait")
            })?;
            let embedded_in = parent
                .as_deref()
                .and_then(|p| defs.get(p))
                .map(|p| embedding_states(p, &name))
                .unwrap_or_default();
            let (states, transitions) = machine_entry(def);
            let mut entry = ValueMap::default();
            entry.insert("trait".into(), Value::string(name.clone()));
            entry.insert(
                "orbital".into(),
                opt_string(owners.owning_orbital(&name).ok().map(str::to_string)),
            );
            entry.insert("parent".into(), opt_string(parent.clone()));
            entry.insert("depth".into(), Value::from(depth as i64));
            entry.insert("embeddedIn".into(), names(embedded_in));
            entry.insert("renders".into(), Value::Bool(renders(def)));
            entry.insert("observer".into(), Value::Bool(observers.contains(&name)));
            let browser = def
                .dispatch_mode(None, &orbital_core_base::runs_on::Residence::none())
                .steps_in_browser(self.effect_executor.environment().is_client());
            entry.insert(
                "stepsIn".into(),
                Value::string(if browser { "browser" } else { "server" }),
            );
            entry.insert("linkedEntity".into(), opt_string(def.linked_entity.clone()));
            entry.insert("instances".into(), self.session_instances(&name));
            entry.insert("states".into(), states);
            entry.insert("transitions".into(), transitions);
            entry.insert(
                "emits".into(),
                names(
                    def.emits
                        .iter()
                        .flat_map(|e| e.iter().map(|d| d.event.clone())),
                ),
            );
            let listens = def
                .listens
                .iter()
                .flat_map(|l| l.iter())
                .map(|l| {
                    let mut item = ValueMap::default();
                    item.insert("event".into(), Value::string(l.event.clone()));
                    item.insert("triggers".into(), opt_string(l.triggers.clone()));
                    Value::object(item)
                })
                .collect::<Vec<_>>();
            entry.insert("listens".into(), Value::array(listens));
            out.push(Value::object(entry));
            for child in children(&name).into_iter().rev() {
                if !placed.contains(&child) {
                    stack.push((child, Some(name.clone()), depth + 1));
                }
            }
        }
        let mut answer = ValueMap::default();
        answer.insert("page".into(), Value::string(page.to_string()));
        answer.insert("traits".into(), Value::array(out));
        Ok(Value::object(answer))
    }
}
