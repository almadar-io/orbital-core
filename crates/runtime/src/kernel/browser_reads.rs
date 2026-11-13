//! The fields of each frame the browser reads (EXEC-DIST-8): what a session's delta sends it of a frame. Built once
//! per registration from the trait definitions; `client_view` narrows a delta's ops to it.

use super::*;
use crate::effects::frame_delta::{FrameDelta, FrameOp};
use orbital_core_base::runs_on::{effect_site, Residence, RunsOn};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::schema::types::{split_event_address, ListenSource};

const LIFECYCLE: [&str; 4] = ["INIT", "LOAD", "$MOUNT", "$UNMOUNT"];

/// The fields of a frame the browser reads.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserFields {
    All,
    Named(HashSet<String>),
}

impl BrowserFields {
    pub(super) fn keeps(&self, field: &str) -> bool {
        match self {
            Self::All => true,
            Self::Named(names) => field == "id" || names.contains(field),
        }
    }

    fn union(&mut self, other: &Self) {
        match (&mut *self, other) {
            (Self::All, _) => {}
            (_, Self::All) => *self = Self::All,
            (Self::Named(held), Self::Named(more)) => held.extend(more.iter().cloned()),
        }
    }
}

/// What the browser-site part of an expression reads of `@entity` and which events it names.
#[derive(Default)]
struct Part {
    reads: HashSet<String>,
    whole: bool,
    /// Events the part emits or routes a completion to, on its own trait.
    emits: Vec<String>,
    /// An emit whose event is an expression.
    any_emit: bool,
}

impl Part {
    fn fields(&self) -> BrowserFields {
        if self.whole {
            BrowserFields::All
        } else {
            BrowserFields::Named(self.reads.clone())
        }
    }

    /// Every `@entity` token in `text`, by the binding grammar (`@` root, then `.field`): a bare `@entity` is the
    /// whole row.
    fn read_text(&mut self, text: &str) {
        let bytes = text.as_bytes();
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        for (at, _) in text.match_indices("@entity") {
            if at > 0 && ident(bytes[at - 1]) {
                continue;
            }
            let rest = &bytes[at + "@entity".len()..];
            match rest.first() {
                Some(b) if ident(*b) => {}
                Some(b'.') if rest.get(1).is_some_and(|b| ident(*b)) => {
                    let len = rest[1..].iter().take_while(|b| ident(**b)).count();
                    self.reads.insert(String::from_utf8_lossy(&rest[1..1 + len]).into_owned());
                }
                _ => self.whole = true,
            }
        }
    }

    /// The browser part of `expr`: a server-site effect is the host's, its arguments and routes unread here.
    fn walk(&mut self, expr: &SExpression, residence: &Residence) {
        match expr {
            SExpression::String(text) => self.read_text(text),
            SExpression::Array(items) => {
                let head = items.first().and_then(SExpression::as_str);
                if let Some(op) = head {
                    if op != "render-ui" && effect_site(op, &items[1..], residence) == RunsOn::Server {
                        return;
                    }
                }
                let skip = match (head, items.get(1)) {
                    (Some("set"), Some(SExpression::String(target))) if target.starts_with("@entity") => {
                        // The browser holds what a step it runs writes, so a carried row keeps that field (seed_leg).
                        self.read_text(target);
                        2
                    }
                    (Some("emit"), Some(SExpression::String(event))) if !event.starts_with('@') => {
                        self.emits.push(event.to_string());
                        2
                    }
                    (Some("emit"), Some(_)) => {
                        self.any_emit = true;
                        1
                    }
                    _ => 0,
                };
                items.iter().skip(skip).for_each(|item| self.walk(item, residence));
            }
            SExpression::Object(fields) => {
                if let Some(SExpression::Object(routes)) = fields.get("emit") {
                    for route in routes.values() {
                        match route {
                            SExpression::String(event) if !event.starts_with('@') => self.emits.push(event.to_string()),
                            _ => self.any_emit = true,
                        }
                    }
                }
                fields.values().for_each(|value| self.walk(value, residence));
            }
            _ => {}
        }
    }
}

/// Every `render-ui` in `expr`, wherever it sits.
fn renders<'e>(expr: &'e SExpression, out: &mut Vec<&'e SExpression>) {
    match expr {
        SExpression::Array(items) => {
            if items.first().and_then(SExpression::as_str) == Some("render-ui") {
                out.push(expr);
                return;
            }
            items.iter().for_each(|item| renders(item, out));
        }
        SExpression::Object(fields) => fields.values().for_each(|value| renders(value, out)),
        _ => {}
    }
}

fn listen_source_matches(source: Option<&ListenSource>, emitter: &str) -> bool {
    match source {
        None | Some(ListenSource::Any) => true,
        Some(ListenSource::Trait { trait_name, .. }) | Some(ListenSource::Orbital { trait_name, .. }) => trait_name == emitter,
    }
}

impl RuntimeKernel {
    /// The fields the browser reads of each frame key (a trait's own key, or `$shared::<Entity>`), built on first use
    /// after a registration.
    pub(super) fn browser_reads(&self) -> Arc<HashMap<String, BrowserFields>> {
        if let Some(table) = self.registry.browser_reads.read().as_ref() {
            return Arc::clone(table);
        }
        let table = Arc::new(self.build_browser_reads());
        *self.registry.browser_reads.write() = Some(Arc::clone(&table));
        table
    }

    fn build_browser_reads(&self) -> HashMap<String, BrowserFields> {
        let defs: Vec<Arc<TraitDefinition>> = self.registry.trait_definitions.read().values().cloned().collect();
        let residence = Residence::of_entities(self.registry.entity_definitions.read().values());
        let client_run = |def: &TraitDefinition| self.dispatch_mode_of(def) != DispatchMode::PersistedAwaited;

        let mut ui_events: HashSet<String> = HashSet::new();
        let mut every_event: HashSet<String> = HashSet::new();
        let mut render_reads: HashMap<String, Part> = HashMap::new();
        let mut steps: HashMap<String, Vec<(String, Part)>> = HashMap::new();
        // (emitter or None for any, event) -> (listener, triggered event)
        let mut listens: Vec<(Option<ListenSource>, String, String, String)> = Vec::new();
        for def in &defs {
            let mut painted = Part::default();
            let mut dynamic_ui = false;
            let mut found = Vec::new();
            for transition in &def.state_machine.transitions {
                transition.effects.iter().flatten().for_each(|e| renders(e, &mut found));
            }
            def.ticks.iter().flatten().flat_map(|t| t.effects.iter()).for_each(|e| renders(e, &mut found));
            for render in found {
                painted.walk(render, &residence);
                if let Ok(json) = serde_json::to_value(render) {
                    Self::pattern_event_props(&json, &mut ui_events, &mut dynamic_ui);
                }
            }
            if dynamic_ui {
                every_event.insert(def.name.clone());
            }
            render_reads.insert(def.name.clone(), painted);
            let held: Vec<(String, Part)> = def
                .state_machine
                .transitions
                .iter()
                .map(|transition| {
                    let mut part = Part::default();
                    transition.guard.iter().for_each(|g| part.walk(g, &residence));
                    transition.effects.iter().flatten().for_each(|e| part.walk(e, &residence));
                    transition.ensures.iter().flatten().for_each(|e| part.walk(e, &residence));
                    (transition.event.clone(), part)
                })
                .collect();
            steps.insert(def.name.clone(), held);
            for listen in def.listens.iter().flat_map(|l| l.iter()) {
                let (event, legacy) = split_event_address(&listen.event);
                let source = listen.source.clone().or(legacy);
                let triggers = listen.triggers.clone().unwrap_or_else(|| event.clone());
                listens.push((source, event, def.name.clone(), triggers));
            }
        }

        let mut reachable: HashSet<(String, String)> = HashSet::new();
        let mut work: Vec<(String, String)> = Vec::new();
        let mut reach = |trait_name: &str, event: &str, work: &mut Vec<(String, String)>| {
            if reachable.insert((trait_name.to_string(), event.to_string())) {
                work.push((trait_name.to_string(), event.to_string()));
            }
        };
        for def in &defs {
            let all = client_run(def) || every_event.contains(&def.name);
            for (event, _) in steps.get(&def.name).into_iter().flatten() {
                if all || LIFECYCLE.contains(&event.as_str()) || ui_events.contains(event) {
                    reach(&def.name, event, &mut work);
                }
            }
        }
        let mut emitted_any: HashSet<String> = HashSet::new();
        let mut client_ticks: Vec<(String, Part)> = Vec::new();
        for def in defs.iter().filter(|d| client_run(d)) {
            for tick in def.ticks.iter().flatten() {
                let mut part = Part::default();
                tick.guard.iter().for_each(|g| part.walk(g, &residence));
                tick.effects.iter().for_each(|e| part.walk(e, &residence));
                client_ticks.push((def.name.clone(), part));
            }
        }
        let mut follow = |emitter: &str, part: &Part, work: &mut Vec<(String, String)>, emitted_any: &mut HashSet<String>| {
            for event in &part.emits {
                reach(emitter, event, work);
                for (source, heard, listener, triggers) in &listens {
                    if heard == event && listen_source_matches(source.as_ref(), emitter) {
                        reach(listener, triggers, work);
                    }
                }
            }
            if part.any_emit && emitted_any.insert(emitter.to_string()) {
                for (event, _) in steps.get(emitter).into_iter().flatten() {
                    reach(emitter, event, work);
                }
                for (source, _, listener, triggers) in &listens {
                    if listen_source_matches(source.as_ref(), emitter) {
                        reach(listener, triggers, work);
                    }
                }
            }
        };
        for (emitter, part) in &client_ticks {
            follow(emitter, part, &mut work, &mut emitted_any);
        }
        while let Some((trait_name, event)) = work.pop() {
            for (step_event, part) in steps.get(&trait_name).into_iter().flatten() {
                if *step_event == event {
                    follow(&trait_name, part, &mut work, &mut emitted_any);
                }
            }
        }

        let mut by_trait: HashMap<String, BrowserFields> = HashMap::new();
        for def in &defs {
            let mut fields = render_reads.get(&def.name).map_or(BrowserFields::Named(HashSet::new()), Part::fields);
            let mut ran = false;
            for (event, part) in steps.get(&def.name).into_iter().flatten() {
                if reachable.contains(&(def.name.clone(), event.clone())) {
                    ran = true;
                    fields.union(&part.fields());
                }
            }
            for (name, part) in &client_ticks {
                if *name == def.name {
                    ran = true;
                    fields.union(&part.fields());
                }
            }
            for listen in def.listens.iter().flat_map(|l| l.iter()) {
                let triggers = listen.triggers.clone().unwrap_or_else(|| split_event_address(&listen.event).0);
                if reachable.contains(&(def.name.clone(), triggers)) {
                    let mut part = Part::default();
                    listen.guard.iter().for_each(|g| part.walk(g, &residence));
                    listen.key.iter().for_each(|k| part.walk(k, &residence));
                    if let Some(mapping) = &listen.payload_mapping {
                        mapping.values().for_each(|v| part.walk(v, &residence));
                    }
                    fields.union(&part.fields());
                }
            }
            if ran {
                let mut part = Part::default();
                def.state_machine.states.iter().filter_map(|s| s.holds.as_ref()).for_each(|h| part.walk(h, &residence));
                fields.union(&part.fields());
            }
            by_trait.insert(def.name.clone(), fields);
        }

        let mut table: HashMap<String, BrowserFields> = HashMap::new();
        for def in &defs {
            let fields = &by_trait[&def.name];
            table.entry(self.trait_frame_key(&def.name)).or_insert_with(|| BrowserFields::Named(HashSet::new())).union(fields);
            table.entry(def.name.clone()).or_insert_with(|| BrowserFields::Named(HashSet::new())).union(fields);
        }
        table
    }

    /// `delta` narrowed to what the browser reads (EXEC-DIST-8): only a session's delta, since an unversioned one
    /// is the state a stateless host's browser carries back.
    pub(super) fn browser_frames(&self, delta: &mut FrameDelta) {
        if delta.session.is_none() {
            return;
        }
        let table = self.browser_reads();
        let keep = |frame: &str, fields: &mut ValueMap| {
            let key = frame.split('\u{1e}').next().unwrap_or(frame);
            if let Some(read) = table.get(key) {
                fields.retain(|field, _| read.keeps(field.as_str()));
            }
        };
        for op in &mut delta.ops {
            match op {
                FrameOp::Put { frame, fields } => keep(frame, fields),
                FrameOp::Set { frames } => frames.iter_mut().for_each(|(frame, fields)| keep(frame, fields)),
                FrameOp::Del { .. } | FrameOp::Clear => {}
            }
        }
        delta.ops.retain(|op| !matches!(op, FrameOp::Set { frames } if frames.iter().all(|(_, f)| f.is_empty())));
        for op in &mut delta.ops {
            if let FrameOp::Set { frames } = op {
                frames.retain(|(_, fields)| !fields.is_empty());
            }
        }
    }
}
