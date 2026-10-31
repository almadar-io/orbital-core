//! The static `os/` grant check over a lowered program (`program/os-grants`): every `os/` call is covered by the
//! grants of the orbital that makes it (app level plus the orbital's own), its targets are literals, and a declared
//! entry is well formed and used. The requirement derivation and the coverage are this module's siblings; this one
//! walks the program and answers typed findings, and the std lint words them.

use super::{
    anchored_sexpr, concat_static_prefix, os_requirements_deferring, DevResource, OsGrants, OsRequirement,
};
use crate::collections::IndexMap;
use crate::os::{split_envelope, OsOp};
use crate::schema::{ConfigField, OrbitalSchema, SExpression, TraitDefinition, TraitReference};
use serde::{Deserialize, Serialize};

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsGrantFindingCode {
    Missing,
    Unused,
    EntryInvalid,
    TargetNotStatic,
}

/// One finding. `entry` is what the call needs (`missing`), the granted entry (`unused`) or the problem
/// (`entry-invalid`); `grant` is the entry to add for a `missing` one and `app` says the app-level bracket takes it
/// too; `argument` is the 1-based argument of a `target-not-static` call; `via` says how a finding of the finished
/// program arose (`config`, `config-unresolvable`, `computed-path`) and is absent for one of the program as written.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsGrantFinding {
    pub code: OsGrantFindingCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orbital: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    pub site: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<OsGrantVia>,
}

/// How a finding of the finished program arose.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsGrantVia {
    Config,
    ConfigUnresolvable,
    ComputedPath,
}

/// What `program/os-grants` answers: the findings of one pass.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsGrantsReport {
    pub findings: Vec<OsGrantFinding>,
}

/// Which pass: the program as written, or the finished program, where a config-fed target is a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsGrantPass {
    Written,
    Final,
}

fn each_os_call<'a>(expr: &'a SExpression, visit: &mut dyn FnMut(OsOp, &'a [SExpression])) {
    match expr {
        SExpression::Array(arr) => {
            if let Some(SExpression::String(head)) = arr.first() {
                if let Some(op) = OsOp::from_name(head) {
                    visit(op, &arr[1..]);
                }
            }
            for item in arr {
                each_os_call(item, visit);
            }
        }
        SExpression::Object(obj) => {
            for v in obj.values() {
                each_os_call(v, visit);
            }
        }
        _ => {}
    }
}

/// A target read from the composing call site's `config`: the binding `@config.*`, or a call whose data
/// argument (its first) is one.
fn config_rooted(expr: &SExpression) -> bool {
    match expr {
        SExpression::Array(arr) if expr.is_call() => arr.get(1).is_some_and(config_rooted),
        _ => expr
            .binding_path()
            .is_some_and(|path| path.split('.').next() == Some("config")),
    }
}

/// The `path` of an anchored argument when it depends on the composing call site's config.
fn deferred_anchored_path(arg: &SExpression) -> Option<&SExpression> {
    let (_, path) = anchored_sexpr(arg).ok().flatten()?;
    let path = path?;
    let concat_has_config = matches!(path, SExpression::Array(pieces)
        if pieces.first().and_then(SExpression::as_str) == Some("str/concat") && pieces[1..].iter().any(config_rooted));
    (config_rooted(path) || concat_has_config).then_some(path)
}

/// What the calls of `op` could still require once their deferred anchored paths are known: each such path read
/// as far as its leading literal directory, so a grant a deferred call can reach is not unused.
fn deferred_reach(op: OsOp, positional: &[SExpression]) -> Vec<OsRequirement> {
    let mut pieces: Vec<&SExpression> = Vec::new();
    for arg in positional {
        let candidates: Vec<&SExpression> = match arg {
            SExpression::Object(fields) if anchored_sexpr(arg).ok().flatten().is_none() => {
                fields.values().collect()
            }
            _ => vec![arg],
        };
        for candidate in candidates {
            if let Some(path) = deferred_anchored_path(candidate) {
                pieces.push(path);
                if let SExpression::Array(parts) = path {
                    pieces.extend(parts[1..].iter());
                }
            }
        }
    }
    if pieces.is_empty() {
        return Vec::new();
    }
    os_requirements_deferring(op, positional, &|expr| {
        config_rooted(expr) && !pieces.iter().any(|piece| std::ptr::eq(*piece, expr))
    })
    .unwrap_or_default()
}

/// Every path an anchored `path` can take in the finished program: each config piece replaced by each value its
/// knob can hold. `None` when a piece cannot be read as text.
fn anchored_path_values(
    path: &SExpression,
    config: &IndexMap<String, ConfigField>,
) -> Option<Vec<SExpression>> {
    let readable = |value: &SExpression| matches!(value, SExpression::String(s) if !s.starts_with('@'));
    let SExpression::Array(arr) = path else {
        let values = config_values(path, config)?;
        return values.iter().all(readable).then_some(values);
    };
    if arr.first().and_then(SExpression::as_str) != Some("str/concat") {
        let values = config_values(path, config)?;
        return values.iter().all(readable).then_some(values);
    }
    let mut built: Vec<Vec<SExpression>> = vec![vec![SExpression::string("str/concat")]];
    for piece in &arr[1..] {
        let options = if config_rooted(piece) {
            config_values(piece, config)?
        } else {
            vec![piece.clone()]
        };
        if !options.iter().all(|o| readable(o) || !config_rooted(piece)) {
            return None;
        }
        let mut next = Vec::new();
        for head in &built {
            for option in &options {
                let mut grown = head.clone();
                grown.push(option.clone());
                next.push(grown);
            }
        }
        built = next;
    }
    Some(built.into_iter().map(SExpression::array).collect())
}

fn effect_sites<'a>(trait_def: &'a TraitDefinition, path: &str) -> Vec<(&'a SExpression, String)> {
    let mut sites = Vec::new();
    for (i, transition) in trait_def.state_machine.transitions.iter().enumerate() {
        if transition
            .guard
            .as_ref()
            .is_some_and(SExpression::is_statically_false)
        {
            continue;
        }
        for (j, effect) in transition.effects.iter().flatten().enumerate() {
            sites.push((
                effect,
                format!("{path}.stateMachine.transitions[{i}].effects[{j}]"),
            ));
        }
    }
    for (i, tick) in trait_def.ticks.iter().flatten().enumerate() {
        for (j, effect) in tick.effects.iter().enumerate() {
            sites.push((effect, format!("{path}.ticks[{i}].effects[{j}]")));
        }
    }
    sites
}

fn trait_path(
    orbital_index: usize,
    orbital_name: &str,
    trait_index: usize,
    trait_ref: &TraitReference,
) -> String {
    let name = match trait_ref {
        TraitReference::Inline(td) => td.name.as_str(),
        TraitReference::Reference {
            name, reference, ..
        } => name
            .as_deref()
            .or(reference.as_deref())
            .unwrap_or("<anonymous>"),
    };
    format!("orbitals[{orbital_index} ({orbital_name})].traits[{trait_index} ({name})]")
}

fn describe(need: &OsRequirement) -> String {
    match need {
        OsRequirement::Fs { path, mode } => format!("fs {path:?} {}", mode.name()),
        OsRequirement::FsTree { root, mode } => format!("fs \"{root}**\" {}", mode.name()),
        OsRequirement::FsAnchored { anchor, path, mode } => {
            format!("fs {} {path:?} {}", anchor.label(), mode.name())
        }
        OsRequirement::FsAnchoredTree { anchor, root, mode } => {
            format!("fs {} \"{root}**\" {}", anchor.label(), mode.name())
        }
        OsRequirement::Exec { command } => format!("exec {command:?}"),
        OsRequirement::ExecAnchored { anchor, path } => match path {
            Some(path) => format!("exec {} {path:?}", anchor.label()),
            None => format!("exec {}", anchor.label()),
        },
        OsRequirement::Net { host, port, kinds } => {
            let kinds: Vec<&str> = kinds.iter().map(|k| k.name()).collect();
            format!("net \"{host}:{port}\" {}", kinds.join(" or "))
        }
        OsRequirement::NetAnchored { host, port, kinds } => {
            let kinds: Vec<&str> = kinds.iter().map(|k| k.name()).collect();
            format!(
                "net {}, {} {}",
                host.label(),
                port.label(),
                kinds.join(" or ")
            )
        }
        OsRequirement::Env { name } => format!("env {name:?}"),
        OsRequirement::Stdio { kind } => format!("stdio {}", kind.name()),
        OsRequirement::Sys { kind } => format!("sys {}", kind.name()),
        OsRequirement::Desktop { kind } => format!("desktop {}", kind.name()),
        OsRequirement::Kv { store, mode } => format!("kv {store:?} {}", mode.name()),
        OsRequirement::Dev { resource, kind } => {
            format!("dev {} {}", dev_resource_label(resource), kind.name())
        }
    }
}

/// The grant entry that would cover `need`, and whether the app-level bracket takes it as well as the orbital's.
fn grant_entry(need: &OsRequirement) -> (String, bool) {
    match need {
        OsRequirement::Fs { path, mode } => {
            (format!("fs: {{ {path:?}: [{}] }}", mode.name()), true)
        }
        OsRequirement::FsTree { root, mode } => {
            (format!("fs: {{ \"{root}**\": [{}] }}", mode.name()), true)
        }
        OsRequirement::FsAnchored { anchor, path, mode } => (
            format!("fs: {{ {}: {path:?}: [{}] }}", anchor.label(), mode.name()),
            true,
        ),
        OsRequirement::FsAnchoredTree { anchor, root, mode } => (
            format!(
                "fs: {{ {}: \"{root}**\": [{}] }}",
                anchor.label(),
                mode.name()
            ),
            true,
        ),
        OsRequirement::Exec { command } => (format!("exec: [{command:?}]"), false),
        OsRequirement::ExecAnchored { anchor, path } => match path {
            Some(path) => (format!("exec: [{} {path:?}]", anchor.label()), false),
            None => (format!("exec: [{}]", anchor.label()), false),
        },
        OsRequirement::NetAnchored { host, port, kinds } => (
            format!(
                "net: {{ {}, {}: [{}] }}",
                host.label(),
                port.label(),
                kinds.first().map_or("tcp", |k| k.name())
            ),
            false,
        ),
        OsRequirement::Net { host, port, kinds } => (
            format!(
                "net: {{ \"{host}:{port}\": [{}] }}",
                kinds.first().map_or("tcp", |k| k.name())
            ),
            false,
        ),
        OsRequirement::Env { name } => (format!("env: [{name:?}]"), false),
        OsRequirement::Stdio { kind } => (format!("stdio: [{}]", kind.name()), false),
        OsRequirement::Sys { kind } => (format!("sys: [{}]", kind.name()), false),
        OsRequirement::Desktop { kind } => (format!("desktop: [{}]", kind.name()), false),
        OsRequirement::Dev { resource, kind } => (
            format!(
                "dev: {{ \"{}\": [{}] }}",
                dev_grant_target(resource),
                kind.name()
            ),
            true,
        ),
        OsRequirement::Kv { store, mode } => {
            (format!("kv: {{ {store:?}: [{}] }}", mode.name()), true)
        }
    }
}

fn dev_resource_label(resource: &DevResource) -> String {
    match resource {
        DevResource::Mmio {
            base,
            offset,
            width,
        } => format!("mmio {base:#x}+{offset:#x} ({width} bytes)"),
        DevResource::Irq { line } => format!("irq {line}"),
    }
}

fn dev_grant_target(resource: &DevResource) -> String {
    match resource {
        DevResource::Mmio {
            base,
            offset,
            width,
        } => format!("mmio:{base:#x}+{:#x}", offset + width),
        DevResource::Irq { line } => format!("irq:{line}"),
    }
}

fn used_by_any(atom: &OsGrants, needs: &[OsRequirement]) -> bool {
    needs.iter().any(|need| atom.allows(need))
}

fn missing(
    orbital: &str,
    op: OsOp,
    need: &OsRequirement,
    site: &str,
    via: Option<OsGrantVia>,
) -> OsGrantFinding {
    let (grant, app) = grant_entry(need);
    OsGrantFinding {
        code: OsGrantFindingCode::Missing,
        orbital: Some(orbital.to_string()),
        call: Some(op.name().to_string()),
        entry: Some(describe(need)),
        site: site.to_string(),
        grant: Some(grant),
        app: Some(app),
        argument: None,
        via,
    }
}

fn not_static(
    orbital: &str,
    op: OsOp,
    argument: usize,
    site: &str,
    via: Option<OsGrantVia>,
) -> OsGrantFinding {
    OsGrantFinding {
        code: OsGrantFindingCode::TargetNotStatic,
        orbital: Some(orbital.to_string()),
        call: Some(op.name().to_string()),
        entry: None,
        site: site.to_string(),
        grant: None,
        app: None,
        argument: Some(argument),
        via,
    }
}

fn unused(site: String, label: &str, orbital: Option<&str>) -> OsGrantFinding {
    OsGrantFinding {
        code: OsGrantFindingCode::Unused,
        orbital: orbital.map(str::to_string),
        call: None,
        entry: Some(label.to_string()),
        site,
        grant: None,
        app: None,
        argument: None,
        via: None,
    }
}

fn check_entries(
    grants: &OsGrants,
    at: &str,
    orbital: Option<&str>,
    out: &mut Vec<OsGrantFinding>,
) {
    for problem in grants.entry_problems() {
        out.push(OsGrantFinding {
            code: OsGrantFindingCode::EntryInvalid,
            orbital: orbital.map(str::to_string),
            call: None,
            entry: Some(problem.message),
            site: format!("{at}.{}", problem.path),
            grant: None,
            app: None,
            argument: None,
            via: None,
        });
    }
}

/// The findings of the program as written: an `os/` call not covered by the grants of its orbital, a computed
/// target, a malformed entry, an entry no call uses.
fn written(schema: &OrbitalSchema) -> Vec<OsGrantFinding> {
    let mut out = Vec::new();
    if let Some(app) = &schema.os_grants {
        check_entries(app, "osGrants", None, &mut out);
    }
    let mut needs_by_orbital: Vec<Vec<OsRequirement>> = Vec::new();
    let mut reach_by_orbital: Vec<Vec<OsRequirement>> = Vec::new();
    for (i, orbital) in schema.orbitals.iter().enumerate() {
        let at = format!("orbitals[{i} ({})]", orbital.name);
        if let Some(own) = &orbital.os_grants {
            check_entries(
                own,
                &format!("{at}.osGrants"),
                Some(&orbital.name),
                &mut out,
            );
        }
        let grants = schema.orbital_os_grants(orbital);
        let mut needs: Vec<OsRequirement> = Vec::new();
        let mut reach: Vec<OsRequirement> = Vec::new();
        for (j, trait_ref) in orbital.traits.iter().enumerate() {
            let TraitReference::Inline(trait_def) = trait_ref else {
                continue;
            };
            let path = trait_path(i, &orbital.name, j, trait_ref);
            for (effect, site) in effect_sites(trait_def, &path) {
                each_os_call(effect, &mut |op, args| {
                    let (positional, _) = split_envelope(op, args);
                    reach.extend(deferred_reach(op, positional));
                    match os_requirements_deferring(op, positional, &config_rooted) {
                        Err(arg) => out.push(not_static(&orbital.name, op, arg + 1, &site, None)),
                        Ok(found) => {
                            for need in found {
                                if !grants.allows(&need) {
                                    out.push(missing(&orbital.name, op, &need, &site, None));
                                }
                                needs.push(need);
                            }
                        }
                    }
                });
            }
        }
        if let Some(own) = &orbital.os_grants {
            for atom in own.atoms() {
                if !used_by_any(&atom.grants, &needs) && !used_by_any(&atom.grants, &reach) {
                    out.push(unused(
                        format!("{at}.osGrants.{}", atom.path),
                        &atom.label,
                        Some(&orbital.name),
                    ));
                }
            }
        }
        needs_by_orbital.push(needs);
        reach_by_orbital.push(reach);
    }
    if let Some(app) = &schema.os_grants {
        let all: Vec<OsRequirement> = needs_by_orbital.into_iter().flatten().collect();
        let all_reach: Vec<OsRequirement> = reach_by_orbital.into_iter().flatten().collect();
        for atom in app.atoms() {
            if !used_by_any(&atom.grants, &all) && !used_by_any(&atom.grants, &all_reach) {
                out.push(unused(format!("osGrants.{}", atom.path), &atom.label, None));
            }
        }
    }
    out
}

/// The values a config-fed `os/` target can take in the finished program, read from the trait's composed knob
/// defaults. `@config.k.p` reads a knob and a path into it; `array/nth` with a literal index takes that element and
/// with any other index every element; `object/get` with a literal key takes that field of each object. `None` for
/// anything else; a knob with no value yields no candidate.
fn config_values(
    expr: &SExpression,
    config: &IndexMap<String, ConfigField>,
) -> Option<Vec<SExpression>> {
    if let SExpression::Array(arr) = expr {
        if expr.is_call() {
            return match arr.first().and_then(SExpression::as_str)? {
                "array/nth" | "nth" => {
                    let mut found = Vec::new();
                    for source in config_values(arr.get(1)?, config)? {
                        let SExpression::Array(items) = source else {
                            return None;
                        };
                        match arr.get(2) {
                            Some(SExpression::Number(at)) if *at >= 0.0 && at.fract() == 0.0 => {
                                found.extend(items.get(*at as usize).cloned());
                            }
                            _ => found.extend(items),
                        }
                    }
                    Some(found)
                }
                "object/get" => {
                    let key = arr.get(2)?.as_str()?;
                    let mut found = Vec::new();
                    for source in config_values(arr.get(1)?, config)? {
                        let SExpression::Object(fields) = source else {
                            return None;
                        };
                        found.extend(fields.get(key).cloned());
                    }
                    Some(found)
                }
                _ => None,
            };
        }
    }
    let path = expr.binding_path()?;
    let mut parts = path.split('.');
    if parts.next() != Some("config") {
        return None;
    }
    let Some(mut value) = config.get(parts.next()?)?.default.clone() else {
        return Some(Vec::new());
    };
    for part in parts {
        let SExpression::Object(fields) = value else {
            return None;
        };
        value = fields.get(part)?.clone();
    }
    Some(vec![value])
}

/// The directory prefixes a `str/concat` file target can take in the finished program: its static prefix with
/// every config piece replaced by each string the knob can hold, as a `str/concat` of literals.
fn concat_roots(
    expr: &SExpression,
    config: &IndexMap<String, ConfigField>,
) -> Option<Vec<SExpression>> {
    let mut roots: Vec<Vec<SExpression>> = vec![vec![SExpression::string("str/concat")]];
    for piece in concat_static_prefix(expr, &config_rooted)? {
        let options = if config_rooted(piece) {
            config_values(piece, config)?
        } else {
            vec![piece.clone()]
        };
        let mut next = Vec::new();
        for root in &roots {
            for option in &options {
                match option {
                    SExpression::String(text) if !text.starts_with('@') => {
                        let mut grown = root.clone();
                        grown.push(option.clone());
                        next.push(grown);
                    }
                    _ => return None,
                }
            }
        }
        roots = next;
    }
    Some(roots.into_iter().map(SExpression::array).collect())
}

/// The findings of the finished program: a config-fed `os/` target is a literal once the program is composed, so
/// every value it can take is checked against the orbital's grants like a written one; one the pass cannot resolve
/// is not static.
fn finished(schema: &OrbitalSchema) -> Vec<OsGrantFinding> {
    let mut out = Vec::new();
    for (i, orbital) in schema.orbitals.iter().enumerate() {
        let grants = schema.orbital_os_grants(orbital);
        for (j, trait_ref) in orbital.traits.iter().enumerate() {
            let TraitReference::Inline(trait_def) = trait_ref else {
                continue;
            };
            let config = trait_def.config.clone().unwrap_or_default();
            let path = trait_path(i, &orbital.name, j, trait_ref);
            for (effect, site) in effect_sites(trait_def, &path) {
                each_os_call(effect, &mut |op, args| {
                    let (positional, _) = split_envelope(op, args);
                    let as_written = os_requirements_deferring(op, positional, &config_rooted)
                        .unwrap_or_default();
                    for (at, arg) in positional.iter().enumerate() {
                        let Some(path) = deferred_anchored_path(arg) else {
                            continue;
                        };
                        let SExpression::Object(fields) = arg else {
                            continue;
                        };
                        let Some(values) = anchored_path_values(path, &config) else {
                            out.push(not_static(
                                &orbital.name,
                                op,
                                at + 1,
                                &site,
                                Some(OsGrantVia::ConfigUnresolvable),
                            ));
                            continue;
                        };
                        for value in values {
                            let mut fields = fields.clone();
                            fields.insert("path".to_string().into(), value);
                            let mut substituted = positional.to_vec();
                            substituted[at] = SExpression::Object(fields);
                            let Ok(found) =
                                os_requirements_deferring(op, &substituted, &config_rooted)
                            else {
                                continue;
                            };
                            for need in found.into_iter().filter(|need| !as_written.contains(need))
                            {
                                if !grants.allows(&need) {
                                    out.push(missing(
                                        &orbital.name,
                                        op,
                                        &need,
                                        &site,
                                        Some(OsGrantVia::Config),
                                    ));
                                }
                            }
                        }
                    }
                    for (at, arg) in positional.iter().enumerate().filter(|(_, a)| {
                        config_rooted(a)
                            || concat_static_prefix(a, &config_rooted)
                                .is_some_and(|prefix| prefix.iter().any(config_rooted))
                    }) {
                        let concat = concat_static_prefix(arg, &config_rooted).is_some();
                        let resolved = if concat {
                            concat_roots(arg, &config)
                        } else {
                            config_values(arg, &config)
                        };
                        let Some(values) = resolved else {
                            out.push(not_static(
                                &orbital.name,
                                op,
                                at + 1,
                                &site,
                                Some(OsGrantVia::ConfigUnresolvable),
                            ));
                            continue;
                        };
                        for value in values {
                            let mut substituted = positional.to_vec();
                            substituted[at] = value;
                            let found =
                                match os_requirements_deferring(op, &substituted, &config_rooted) {
                                    Ok(found) if !(concat && found.is_empty()) => found,
                                    Err(_) if !concat => continue,
                                    _ => {
                                        out.push(not_static(
                                            &orbital.name,
                                            op,
                                            at + 1,
                                            &site,
                                            Some(OsGrantVia::ComputedPath),
                                        ));
                                        break;
                                    }
                                };
                            for need in found.into_iter().filter(|need| !as_written.contains(need))
                            {
                                if !grants.allows(&need) {
                                    out.push(missing(
                                        &orbital.name,
                                        op,
                                        &need,
                                        &site,
                                        Some(OsGrantVia::Config),
                                    ));
                                }
                            }
                        }
                    }
                });
            }
        }
    }
    out
}

/// The typed findings of one pass of the static `os/` grant check over a lowered program.
pub fn os_grant_findings(schema: &OrbitalSchema, pass: OsGrantPass) -> Vec<OsGrantFinding> {
    match pass {
        OsGrantPass::Written => written(schema),
        OsGrantPass::Final => finished(schema),
    }
}
