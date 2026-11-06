//! The static bind-group check (EXEC-GPU-6): a `gpu/bind-group-create`'s entries against the bindings its pipeline's
//! entry points use in its WGSL. The pipeline is a sealed handle, so it is traced back through the trait: `@payload.F`
//! of an event is the minted field of the calls whose success route that event is, and `@entity.F` is whatever each
//! `set` of the field wrote. A handle any step of that trace cannot follow (another trait's emit, a listen, a computed
//! value) is left to the run-time check, so a finding is always a pipeline the program really builds.

use super::static_check::{config_values, OsGrantFinding, OsGrantFindingCode};
use crate::collections::IndexMap;
use crate::gpu_wgsl::{check_wgsl, WgslResource};
use crate::os::{split_envelope, OsOp};
use crate::schema::{ConfigField, OrbitalSchema, SExpression, TraitDefinition, TraitReference};

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

struct Call<'a> {
    op: OsOp,
    positional: &'a [SExpression],
    success: Option<&'a str>,
    event: Option<&'a str>,
    site: String,
}

struct Trace<'a> {
    calls: Vec<Call<'a>>,
    sets: Vec<(&'a str, &'a SExpression, Option<&'a str>)>,
    arriving: Vec<&'a str>,
    config: IndexMap<String, ConfigField>,
}

fn walk<'a>(expr: &'a SExpression, event: Option<&'a str>, site: &str, trace: &mut Trace<'a>) {
    match expr {
        SExpression::Array(arr) => {
            match arr.first() {
                Some(SExpression::String(head)) if head == "set" => {
                    if let (Some(field), Some(value)) =
                        (arr.get(1).and_then(SExpression::binding_path), arr.get(2))
                    {
                        if let Some(field) = field.strip_prefix("entity.") {
                            trace.sets.push((field, value, event));
                        }
                    }
                }
                Some(SExpression::String(head)) if head == "emit" => {
                    if let Some(name) = arr.get(1).and_then(SExpression::as_str) {
                        trace.arriving.push(name);
                    }
                }
                Some(SExpression::String(head)) => {
                    if let Some(op) = OsOp::from_name(head) {
                        let (positional, envelope) = split_envelope(op, &arr[1..]);
                        let success =
                            envelope
                                .and_then(|options| options.get("emit"))
                                .and_then(|emit| match emit {
                                    SExpression::Object(routes) => {
                                        routes.get("success").and_then(SExpression::as_str)
                                    }
                                    _ => None,
                                });
                        trace.calls.push(Call {
                            op,
                            positional,
                            success,
                            event,
                            site: site.to_string(),
                        });
                    }
                }
                _ => {}
            }
            for item in arr {
                walk(item, event, site, trace);
            }
        }
        SExpression::Object(obj) => {
            for v in obj.values() {
                walk(v, event, site, trace);
            }
        }
        _ => {}
    }
}

fn trace_of<'a>(trait_def: &'a TraitDefinition, path: &str) -> Trace<'a> {
    let mut trace = Trace {
        calls: Vec::new(),
        sets: Vec::new(),
        arriving: Vec::new(),
        config: trait_def.config.clone().unwrap_or_default(),
    };
    for listen in trait_def.listens.iter().flat_map(|l| l.iter()) {
        trace
            .arriving
            .push(listen.triggers.as_deref().unwrap_or(listen.event.as_str()));
    }
    for (i, transition) in trait_def.state_machine.transitions.iter().enumerate() {
        if transition
            .guard
            .as_ref()
            .is_some_and(SExpression::is_statically_false)
        {
            continue;
        }
        for (j, effect) in transition.effects.iter().flatten().enumerate() {
            walk(
                effect,
                Some(transition.event.as_str()),
                &format!("{path}.stateMachine.transitions[{i}].effects[{j}]"),
                &mut trace,
            );
        }
    }
    for (i, tick) in trait_def.ticks.iter().flatten().enumerate() {
        for (j, effect) in tick.effects.iter().enumerate() {
            walk(
                effect,
                None,
                &format!("{path}.ticks[{i}].effects[{j}]"),
                &mut trace,
            );
        }
    }
    trace
}

impl<'a> Trace<'a> {
    /// The calls of `op` whose minted handle `expr` (read in a step of `event`) can hold; `None` when it cannot be traced.
    fn producers(
        &self,
        expr: &'a SExpression,
        event: Option<&str>,
        op: OsOp,
        seen: &mut Vec<&'a str>,
    ) -> Option<Vec<&Call<'a>>> {
        if matches!(expr, SExpression::Null) {
            return Some(Vec::new());
        }
        let path = expr.binding_path()?;
        if let Some(field) = path.strip_prefix("payload.") {
            let event = event?;
            if self.arriving.contains(&event) || field != minted_field(op) {
                return None;
            }
            return Some(
                self.calls
                    .iter()
                    .filter(|c| c.op == op && c.success == Some(event))
                    .collect(),
            );
        }
        let field = path.strip_prefix("entity.")?;
        if field.contains('.') {
            return None;
        }
        if seen.contains(&field) {
            return Some(Vec::new());
        }
        seen.push(field);
        let mut found = Vec::new();
        for (set, value, at) in &self.sets {
            if *set == field {
                found.extend(self.producers(value, *at, op, seen)?);
            }
        }
        Some(found)
    }

    /// Every text `expr` can hold: a literal, or each value of a knob the finished program composed.
    fn texts(&self, expr: &SExpression) -> Option<Vec<String>> {
        match expr {
            SExpression::String(text) if !text.starts_with('@') => Some(vec![text.to_string()]),
            _ => config_values(expr, &self.config)?
                .into_iter()
                .map(|value| match value {
                    SExpression::String(text) if !text.starts_with('@') => Some(text.to_string()),
                    _ => None,
                })
                .collect(),
        }
    }
}

fn minted_field(op: OsOp) -> &'static str {
    match op {
        OsOp::GpuPipelineCreate => "pipeline",
        OsOp::GpuShaderCreate => "shader",
        _ => "",
    }
}

/// One pipeline a bind group can be made for: its WGSL and the entry points its layout is drawn from.
struct Pipeline {
    wgsl: String,
    entries: Vec<String>,
}

fn pipelines(trace: &Trace, call: &Call) -> Option<Vec<Pipeline>> {
    let mut found = Vec::new();
    for pipeline in trace.producers(
        call.positional.first()?,
        call.event,
        OsOp::GpuPipelineCreate,
        &mut Vec::new(),
    )? {
        let SExpression::Object(desc) = pipeline.positional.get(1)? else {
            return None;
        };
        let mut entry_points = vec![trace.texts(desc.get("entry")?)?];
        if let Some(fragment) = desc.get("fragment") {
            entry_points.push(trace.texts(fragment)?);
        }
        let mut layouts: Vec<Vec<String>> = vec![Vec::new()];
        for options in entry_points {
            layouts = layouts
                .into_iter()
                .flat_map(|head| {
                    options.iter().map(move |name| {
                        let mut grown = head.clone();
                        grown.push(name.clone());
                        grown
                    })
                })
                .collect();
        }
        for shader in trace.producers(
            desc.get("shader")?,
            pipeline.event,
            OsOp::GpuShaderCreate,
            &mut Vec::new(),
        )? {
            for wgsl in trace.texts(shader.positional.get(1)?)? {
                for entries in &layouts {
                    found.push(Pipeline {
                        wgsl: wgsl.clone(),
                        entries: entries.clone(),
                    });
                }
            }
        }
    }
    Some(found)
}

fn resource_name(resource: WgslResource) -> &'static str {
    match resource {
        WgslResource::Buffer(_) => "buffer",
        WgslResource::Texture => "texture",
        WgslResource::Sampler => "sampler",
    }
}

/// The literal entries of a bind group as `(binding, resource)`; `None` when the list or any binding is computed.
fn literal_entries(expr: &SExpression) -> Option<Vec<(u32, &'static str)>> {
    let SExpression::Array(items) = expr else {
        return None;
    };
    let items = match items.first().and_then(SExpression::as_str) {
        Some("list") => &items[1..],
        _ => &items[..],
    };
    items
        .iter()
        .map(|item| {
            let SExpression::Object(fields) = item else {
                return None;
            };
            let binding = match fields.get("binding")? {
                SExpression::Number(n) if *n >= 0.0 && n.fract() == 0.0 => *n as u32,
                _ => return None,
            };
            let kinds: Vec<&'static str> = ["buffer", "texture", "sampler"]
                .into_iter()
                .filter(|k| fields.contains_key(*k))
                .collect();
            match kinds.as_slice() {
                [kind] => Some((binding, *kind)),
                _ => None,
            }
        })
        .collect()
}

/// What is wrong with `given` for group `group` of `pipeline`; empty when they agree. `None` when the WGSL is invalid
/// (the shader check reports that).
fn mismatches(
    pipeline: &Pipeline,
    group: u32,
    given: &[(u32, &'static str)],
) -> Option<Vec<String>> {
    let declared = check_wgsl(&pipeline.wgsl).ok()?;
    let used: Vec<(u32, &'static str)> = declared
        .iter()
        .filter(|b| b.group == group && b.used_by.iter().any(|e| pipeline.entries.contains(e)))
        .map(|b| (b.binding, resource_name(b.resource)))
        .collect();
    let entry = pipeline.entries.join(" + ");
    let mut problems = Vec::new();
    for (binding, kind) in given {
        match used.iter().find(|(b, _)| b == binding) {
            None => problems.push(format!(
                "binding {binding} is not used by group {group} of entry point {entry}"
            )),
            Some((_, wants)) if wants != kind => problems.push(format!(
                "binding {binding} of group {group} is a {wants} in the WGSL, given a {kind}"
            )),
            Some(_) => {}
        }
    }
    for (binding, wants) in &used {
        if !given.iter().any(|(b, _)| b == binding) {
            problems.push(format!("group {group} of entry point {entry} uses binding {binding} ({wants}), which the entries leave out"));
        }
    }
    Some(problems)
}

/// Every `gpu/bind-group-create` of the finished program whose literal entries disagree with a pipeline it is traced to.
pub(super) fn binding_mismatches(schema: &OrbitalSchema) -> Vec<OsGrantFinding> {
    let mut out: Vec<OsGrantFinding> = Vec::new();
    for (i, orbital) in schema.orbitals.iter().enumerate() {
        for (j, trait_ref) in orbital.traits.iter().enumerate() {
            let TraitReference::Inline(trait_def) = trait_ref else {
                continue;
            };
            let path = format!(
                "orbitals[{i} ({})].traits[{j} ({})]",
                orbital.name, trait_def.name
            );
            let trace = trace_of(trait_def, &path);
            for call in trace
                .calls
                .iter()
                .filter(|c| c.op == OsOp::GpuBindGroupCreate)
            {
                let group = match call.positional.get(1) {
                    Some(SExpression::Number(n)) if *n >= 0.0 && n.fract() == 0.0 => *n as u32,
                    _ => continue,
                };
                let Some(given) = call.positional.get(2).and_then(literal_entries) else {
                    continue;
                };
                let Some(candidates) = pipelines(&trace, call) else {
                    continue;
                };
                for pipeline in candidates {
                    let Some(problems) = mismatches(&pipeline, group, &given) else {
                        continue;
                    };
                    if problems.is_empty() {
                        continue;
                    }
                    let finding = OsGrantFinding {
                        code: OsGrantFindingCode::BindingMismatch,
                        orbital: Some(orbital.name.clone()),
                        call: Some(OsOp::GpuBindGroupCreate.name().to_string()),
                        entry: Some(problems.join("; ")),
                        site: call.site.clone(),
                        grant: None,
                        app: None,
                        argument: Some(3),
                        via: None,
                    };
                    if !out.contains(&finding) {
                        out.push(finding);
                    }
                }
            }
        }
    }
    out
}
