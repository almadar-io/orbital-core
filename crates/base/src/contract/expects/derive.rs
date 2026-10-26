//! Deriving a behavior's expectations from its program.

use super::*;

/// Derive the `expects` declarations of `orbital_name` from `schema` (the program's `.orb`
/// JSON). `load` resolves imported behaviors by their `uses` path; without it, imports contribute
/// nothing. Pure; an incompatible shape union is an error.
pub fn derive_expectations(
    schema: &Value,
    orbital_name: &str,
    load: Option<BehaviorLoader<'_>>,
) -> Result<DerivedExpectations, String> {
    let all = orbitals(schema);
    let Some(orbital) = all.iter().find(|o| name_of(o) == Some(orbital_name)) else {
        return Ok(DerivedExpectations {
            expectations: vec![],
            diagnostics: vec![ExpectationDiagnostic {
                kind: DiagnosticKind::UnknownOrbital,
                orbital: orbital_name.to_string(),
                entity: None,
                field: None,
            }],
        });
    };
    let mut diagnostics = Vec::new();

    let mut owner_by_entity: HashMap<String, String> = HashMap::new();
    let mut def_by_entity: HashMap<String, Value> = HashMap::new();
    for o in all {
        for def in inline_entities(o) {
            if let (Some(n), Some(on)) = (name_of(def), name_of(o)) {
                if !owner_by_entity.contains_key(n) {
                    owner_by_entity.insert(n.to_string(), on.to_string());
                    def_by_entity.insert(n.to_string(), def.clone());
                }
            }
        }
    }

    let mut contributed: HashSet<String> = HashSet::new();
    if let Some(load) = load {
        for o in all {
            let by_alias: HashMap<&str, &str> = o
                .get("uses")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|u| Some((u.get("as")?.as_str()?, u.get("from")?.as_str()?)))
                .collect();
            if by_alias.is_empty() {
                continue;
            }
            for t in o
                .get("traits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let r = match t {
                    Value::String(s) => s.as_str(),
                    _ if is_inline_trait(t) => continue,
                    _ => match t.get("ref").and_then(Value::as_str) {
                        Some(r) => r,
                        None => continue,
                    },
                };
                let Some((alias, trait_name)) = parse_imported_trait_ref(r) else {
                    continue;
                };
                let Some(behavior) = by_alias.get(alias).and_then(|b| load(b)) else {
                    continue;
                };
                let Some((bound, aux)) = contributed_entities_of(&behavior, trait_name) else {
                    continue;
                };
                let rebound = t.is_object() && t.get("linkedEntity").is_some();
                let names = if rebound {
                    aux
                } else {
                    bound.into_iter().chain(aux).collect()
                };
                for name in names {
                    if owner_by_entity.contains_key(&name) {
                        continue;
                    }
                    owner_by_entity.insert(name.clone(), name_of(o).unwrap_or("").to_string());
                    contributed.insert(name);
                }
            }
        }
    }

    let mut identity_def: Option<Value> = all
        .iter()
        .filter_map(|o| o.get("entity").and_then(as_entity))
        .find(|e| e.get("identity") == Some(&Value::Bool(true)))
        .cloned();
    if identity_def.is_none() {
        identity_def = all
            .iter()
            .flat_map(|o| inline_entities(o))
            .find(|e| e.get("identity") == Some(&Value::Bool(true)))
            .cloned();
    }
    if identity_def.is_none() {
        if let Some(load) = load {
            identity_def = all
                .iter()
                .find_map(|o| imported_identity_of(o, load, &mut HashSet::new()));
        }
    }

    let mut owner_by_page: BTreeMap<String, String> = BTreeMap::new();
    for o in all {
        for page in o
            .get("pages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(path), Some(on)) = (page.get("path").and_then(Value::as_str), name_of(o)) {
                if !path.is_empty() {
                    owner_by_page
                        .entry(path.to_string())
                        .or_insert_with(|| on.to_string());
                }
            }
        }
    }
    let page_paths: BTreeSet<String> = owner_by_page.keys().cloned().collect();

    let own_defs = inline_entities(orbital);
    let own_names: HashSet<&str> = own_defs.iter().filter_map(|d| name_of(d)).collect();
    let mut w = Walk {
        orbital_name,
        own_defs: own_defs.clone(),
        own_names,
        owner_by_entity: &owner_by_entity,
        owner_by_page: &owner_by_page,
        page_paths: &page_paths,
        user_fields: BTreeSet::new(),
        entity_refs: BTreeMap::new(),
        page_refs: BTreeSet::new(),
    };

    for def in &own_defs {
        for key in [
            "read_policy",
            "create_policy",
            "update_policy",
            "delete_policy",
        ] {
            w.walk(def.get(key));
        }
    }
    for def in &own_defs {
        for field in def
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if field.get("type") == Some(&Value::from("relation")) && field.get("name").is_some() {
                if let Some(target) = field["relation"].get("entity").and_then(Value::as_str) {
                    w.add_entity_ref(target, None);
                }
            }
        }
    }
    for target in orbital
        .get("reference")
        .and_then(|r| r.get("entities"))
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(Value::as_str)
    {
        w.add_entity_ref(target, None);
    }

    let mut events: Vec<Value> = Vec::new();
    let orbital_names: HashSet<&str> = all.iter().filter_map(|o| name_of(o)).collect();
    for t in orbital
        .get("traits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if t.is_string() {
            continue;
        }
        if let Some(le) = t.get("linkedEntity").and_then(Value::as_str) {
            w.add_entity_ref(le, None);
        }
        w.walk_config(t.get("config"));
        if is_inline_trait(t) {
            let sm = t.get("stateMachine");
            for g in sm
                .and_then(|s| s.get("guards"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                w.walk(g.get("expression"));
            }
            for tr in sm
                .and_then(|s| s.get("transitions"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                w.walk(tr.get("guard"));
                w.walk(tr.get("effects"));
            }
            w.walk(t.get("initialEffects"));
            for tick in t
                .get("ticks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                w.walk(tick.get("guard"));
                w.walk(tick.get("effects"));
            }
            for listener in t
                .get("listens")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                w.walk(listener.get("guard"));
                for value in listener
                    .get("payloadMapping")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(|m| m.values())
                {
                    w.walk(Some(value));
                }
            }
        }
        for listener in t
            .get("listens")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let source = listener.get("source");
            if source.and_then(|s| s.get("kind")).and_then(Value::as_str) != Some("orbital") {
                continue;
            }
            let (Some(src_orbital), Some(src_trait), Some(event)) = (
                source
                    .and_then(|s| s.get("orbital"))
                    .and_then(Value::as_str),
                source.and_then(|s| s.get("trait")).and_then(Value::as_str),
                listener.get("event").and_then(Value::as_str),
            ) else {
                continue;
            };
            if src_orbital == orbital_name {
                continue;
            }
            if !orbital_names.contains(src_orbital) {
                diagnostics.push(ExpectationDiagnostic {
                    kind: DiagnosticKind::EventSourceUnknown,
                    orbital: orbital_name.to_string(),
                    entity: None,
                    field: Some(format!("{src_orbital}.{src_trait}.{event}")),
                });
                continue;
            }
            let decl =
                serde_json::json!({ "kind": "event", "traitName": src_trait, "event": event });
            if !events.contains(&decl) {
                events.push(decl);
            }
        }
    }

    // `@user` is one roster per program: an authored or inherited identity expectation naming
    // another roster (an import's own) is the identity this program binds.
    let bound_identity = identity_def.as_ref().and_then(name_of).map(str::to_string);
    let mut expectations: Vec<Value> = Vec::new();
    for mut declared in orbital
        .get("expects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let is_identity = declared.get("kind") == Some(&Value::from("identity"));
        if let (Some(bound), Some(decl)) = (bound_identity.as_deref(), declared.as_object_mut()) {
            if is_identity && decl.contains_key("name") {
                decl.insert("name".into(), Value::from(bound));
            }
        }
        let earlier = expectations
            .iter()
            .position(|e| is_identity && e.get("kind") == Some(&Value::from("identity")));
        match earlier {
            Some(i) => {
                let shape =
                    merge_expectation_shape(expectations[i].get("shape"), declared.get("shape"))?;
                if let (Some(shape), Some(e)) = (shape, expectations[i].as_object_mut()) {
                    e.insert("shape".into(), shape);
                }
            }
            None => expectations.push(declared),
        }
    }

    if let Some(identity) = identity_def.as_ref().filter(|_| !w.user_fields.is_empty()) {
        if let Some(name) = name_of(identity) {
            if let Some(also) = w.entity_refs.remove(name) {
                w.user_fields.extend(also);
            }
        }
    }

    if !w.user_fields.is_empty() {
        let authored_identity = orbital
            .get("expects")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|e| e.get("kind") == Some(&Value::from("identity")));
        let mut shape = Vec::new();
        for field in &w.user_fields {
            let declared = authored_identity
                .and_then(|a| a.get("shape"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|f| name_of(f) == Some(field.as_str()))
                .or_else(|| {
                    identity_def
                        .as_ref()
                        .and_then(|d| d.get("fields"))
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .find(|f| name_of(f) == Some(field.as_str()))
                });
            match declared {
                Some(d) => shape.push(d.clone()),
                None => diagnostics.push(ExpectationDiagnostic {
                    kind: DiagnosticKind::IdentityFieldNotDeclared,
                    orbital: orbital_name.to_string(),
                    entity: identity_def.as_ref().and_then(name_of).map(str::to_string),
                    field: Some(field.clone()),
                }),
            }
        }
        let mut decl = Map::new();
        decl.insert("kind".into(), Value::from("identity"));
        if let Some(n) = identity_def.as_ref().and_then(name_of) {
            decl.insert("name".into(), Value::from(n));
        }
        if !shape.is_empty() {
            decl.insert("shape".into(), Value::Array(shape));
        }
        add_expectation(&mut expectations, Value::Object(decl))?;
    }

    for path in &w.page_refs {
        add_expectation(
            &mut expectations,
            serde_json::json!({ "kind": "page", "path": path }),
        )?;
    }

    for (name, referenced) in &w.entity_refs {
        let provider = def_by_entity.get(name);
        let mut shape = Vec::new();
        for field in referenced {
            match provider
                .and_then(|p| p.get("fields"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|f| name_of(f) == Some(field.as_str()))
            {
                Some(d) => shape.push(d.clone()),
                None if !contributed.contains(name) => diagnostics.push(ExpectationDiagnostic {
                    kind: DiagnosticKind::EntityFieldNotDeclared,
                    orbital: orbital_name.to_string(),
                    entity: Some(name.clone()),
                    field: Some(field.clone()),
                }),
                None => {}
            }
        }
        let mut decl = Map::new();
        decl.insert("kind".into(), Value::from("entity"));
        decl.insert("name".into(), Value::from(name.as_str()));
        if !shape.is_empty() {
            decl.insert("shape".into(), Value::Array(shape));
        }
        add_expectation(&mut expectations, Value::Object(decl))?;
    }

    for event in events {
        if !expectations.contains(&event) {
            expectations.push(event);
        }
    }

    let expectations = expectations
        .into_iter()
        .map(|e| {
            serde_json::from_value::<ExpectDeclaration>(e)
                .map_err(|err| format!("derived expectation is not an ExpectDeclaration: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DerivedExpectations {
        expectations,
        diagnostics,
    })
}
