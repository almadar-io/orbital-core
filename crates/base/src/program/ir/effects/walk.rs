//! Walking an effect: the bindings it reads and its child expressions.

use super::*;

impl OirEffect {
    /// Collect all bindings used in this effect
    pub fn collect_bindings_recursive<'a>(&'a self, bindings: &mut Vec<&'a OirBinding>) {
        match self {
            Self::Set { target, value, .. } => {
                bindings.push(target);
                value
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::SetDynamic { target, value, .. } => {
                // Dynamic target is an expression, collect its bindings
                target
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                value
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Emit { payload, .. } => {
                if let Some(p) = payload {
                    p.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Fetch { id, filter, nearest, .. } => {
                if let Some(n) = nearest {
                    n.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(id_expr) = id {
                    id_expr
                        .collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(f) = filter {
                    f.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Persist { data, .. } => {
                data.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::PersistBatch { ops, .. } => {
                ops.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Ref { id, filter, .. } => {
                if let Some(id_expr) = id {
                    id_expr
                        .collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(f) = filter {
                    f.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Deref { id, .. } => {
                if let Some(id_expr) = id {
                    id_expr
                        .collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Swap { id, transform, .. } => {
                id.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                transform
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Watch { effects, .. } => {
                for eff in effects {
                    eff.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Atomic { effects } => {
                for eff in effects {
                    eff.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Navigate {
                path,
                params,
                crumb,
            } => {
                path.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(p) = params {
                    p.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(c) = crumb {
                    c.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::NavigateBack => {}
            Self::CallService { args, key, .. } => {
                args.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(k) = key {
                    k.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Http {
                origin,
                path,
                request,
                key,
                ..
            } => {
                for expr in [origin, path].into_iter().chain(request).chain(key) {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Redis {
                origin,
                command,
                options,
                key,
                ..
            } => {
                for expr in [origin, command].into_iter().chain(options).chain(key) {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Postgres {
                origin,
                sql,
                params,
                options,
                key,
                ..
            } => {
                for expr in [origin, sql, params].into_iter().chain(options).chain(key) {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::CancelCall { key } => {
                key.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Os { args, key, .. } => {
                for expr in args.iter().chain(key) {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OAuthAuthorize {
                client,
                options,
                key,
                ..
            } => {
                for expr in [Some(client), Some(options), key.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::RandomBytes { bytes, key, .. } => {
                for expr in std::iter::once(bytes).chain(key.as_ref()) {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::CredentialStatus {
                credential: expr, ..
            }
            | Self::CredentialForget {
                credential: expr, ..
            }
            | Self::CredentialAdmin { request: expr, .. }
            | Self::AccessCheck { request: expr, .. }
            | Self::OAuthConnectionStatus { provider: expr, .. }
            | Self::OAuthConnectionForget { provider: expr, .. } => {
                expr.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Spawn { props, .. } => {
                props
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Despawn { entity_id } => {
                entity_id
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Log { message, .. } => {
                message
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Custom { args, .. } => {
                for arg in args.values() {
                    arg.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::RenderUi { .. } => {}
            Self::OsWatchFiles { glob, options, .. } => {
                glob.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(o) = options {
                    o.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OsWatchProcess {
                name, subcommand, ..
            } => {
                name.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(s) = subcommand {
                    s.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OsWatchPort { port, protocol, .. } => {
                port.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(p) = protocol {
                    p.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OsWatchHttp {
                url_pattern,
                method,
                ..
            } => {
                url_pattern
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(m) = method {
                    m.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OsWatchCron { expression, .. } => {
                expression
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::OsWatchSignal { signal, .. } => {
                signal
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::OsWatchEnv { variable, .. } => {
                variable
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::FetchStream { id, filter, .. } => {
                if let Some(id_expr) = id {
                    id_expr
                        .collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(f) = filter {
                    f.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::SendServer { event, payload } => {
                event
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(p) = payload {
                    p.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserOpenFilePicker { options, .. } => {
                if let Some(o) = options {
                    o.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserClipboardRead { .. } => {}
            Self::BrowserPushSubscribe { .. } => {}
            Self::BrowserPageFacts { key, .. } => {
                if let Some(k) = key {
                    k.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserBeacon {
                url, request, key, ..
            } => {
                for e in [Some(url), Some(request), key.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    e.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserPrompt {
                text, schema, key, ..
            } => {
                for e in [Some(text), Some(schema), key.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    e.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserTab { request, key, .. } => {
                for e in [Some(request), key.as_ref()].into_iter().flatten() {
                    e.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserEmbed { texts, key, .. } => {
                for e in [Some(texts), key.as_ref()].into_iter().flatten() {
                    e.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserClipboardWrite { text, .. } => {
                text.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::BrowserGeolocationCurrent { options, .. } => {
                if let Some(o) = options {
                    o.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
        }
    }

    /// Every child expression this effect carries — the one enumeration of
    /// expr-bearing fields, used by `OirExpr::calls_function` to descend
    /// through nested effects. `RenderUi` carries `OirValue` config (render
    /// pipeline), deliberately not part of this surface. Exhaustive match:
    /// a new variant is compiler-forced to declare its expressions here.
    pub fn child_exprs(&self) -> Vec<&OirExpr> {
        let mut out: Vec<&OirExpr> = Vec::new();
        fn push_opt<'a>(out: &mut Vec<&'a OirExpr>, e: &'a Option<OirExpr>) {
            if let Some(e) = e.as_ref() {
                out.push(e);
            }
        }
        match self {
            Self::Set { value, .. } => out.push(value),
            Self::SetDynamic { target, value, .. } => {
                out.push(target);
                out.push(value);
            }
            Self::Emit { payload, .. } => push_opt(&mut out, payload),
            Self::Fetch {
                id,
                filter,
                order_by,
                offset,
                limit,
                nearest,
                ..
            } => {
                push_opt(&mut out, id);
                push_opt(&mut out, filter);
                push_opt(&mut out, order_by);
                push_opt(&mut out, offset);
                push_opt(&mut out, limit);
                push_opt(&mut out, nearest);
            }
            Self::Persist { data, .. } => out.push(data),
            Self::PersistBatch { ops, .. } => out.push(ops),
            Self::Ref {
                id,
                filter,
                order_by,
                ..
            } => {
                push_opt(&mut out, id);
                push_opt(&mut out, filter);
                push_opt(&mut out, order_by);
            }
            Self::Deref { id, .. } => push_opt(&mut out, id),
            Self::Swap { id, transform, .. } => {
                out.push(id);
                out.push(transform);
            }
            Self::Watch { effects, .. } | Self::Atomic { effects } => out.extend(effects.iter()),
            Self::Navigate {
                path,
                params,
                crumb,
            } => {
                out.push(path);
                push_opt(&mut out, params);
                push_opt(&mut out, crumb);
            }
            Self::NavigateBack => {}
            Self::RenderUi { .. } => {}
            Self::CallService { args, key, .. } => {
                out.push(args);
                push_opt(&mut out, key);
            }
            Self::Http {
                origin,
                path,
                request,
                key,
                ..
            } => {
                out.push(origin);
                out.push(path);
                push_opt(&mut out, request);
                push_opt(&mut out, key);
            }
            Self::Redis {
                origin,
                command,
                options,
                key,
                ..
            } => {
                out.push(origin);
                out.push(command);
                push_opt(&mut out, options);
                push_opt(&mut out, key);
            }
            Self::Postgres {
                origin,
                sql,
                params,
                options,
                key,
                ..
            } => {
                out.push(origin);
                out.push(sql);
                out.push(params);
                push_opt(&mut out, options);
                push_opt(&mut out, key);
            }
            Self::CancelCall { key } => out.push(key),
            Self::Os { args, key, .. } => {
                out.extend(args.iter());
                push_opt(&mut out, key);
            }
            Self::RandomBytes { bytes, key, .. } => {
                out.push(bytes);
                push_opt(&mut out, key);
            }
            Self::CredentialStatus {
                credential: expr, ..
            }
            | Self::CredentialForget {
                credential: expr, ..
            }
            | Self::CredentialAdmin { request: expr, .. }
            | Self::AccessCheck { request: expr, .. }
            | Self::OAuthConnectionStatus { provider: expr, .. }
            | Self::OAuthConnectionForget { provider: expr, .. } => out.push(expr),
            Self::OAuthAuthorize {
                client,
                options,
                key,
                ..
            } => {
                out.push(client);
                out.push(options);
                push_opt(&mut out, key);
            }
            Self::Spawn { props, .. } => out.push(props),
            Self::Despawn { entity_id } => out.push(entity_id),
            Self::Log { message, .. } => out.push(message),
            Self::Custom { args, .. } => out.extend(args.values()),
            Self::OsWatchFiles { glob, options, .. } => {
                out.push(glob);
                push_opt(&mut out, options);
            }
            Self::OsWatchProcess {
                name, subcommand, ..
            } => {
                out.push(name);
                push_opt(&mut out, subcommand);
            }
            Self::OsWatchPort { port, protocol, .. } => {
                out.push(port);
                push_opt(&mut out, protocol);
            }
            Self::OsWatchHttp {
                url_pattern,
                method,
                ..
            } => {
                out.push(url_pattern);
                push_opt(&mut out, method);
            }
            Self::OsWatchCron { expression, .. } => out.push(expression),
            Self::OsWatchSignal { signal, .. } => out.push(signal),
            Self::OsWatchEnv { variable, .. } => out.push(variable),
            Self::FetchStream { id, filter, .. } => {
                push_opt(&mut out, id);
                push_opt(&mut out, filter);
            }
            Self::SendServer { event, payload } => {
                out.push(event);
                push_opt(&mut out, payload);
            }
            Self::BrowserOpenFilePicker { options, .. } => push_opt(&mut out, options),
            Self::BrowserClipboardRead { .. } => {}
            Self::BrowserClipboardWrite { text, .. } => out.push(text),
            Self::BrowserGeolocationCurrent { options, .. } => push_opt(&mut out, options),
            Self::BrowserPushSubscribe { .. } => {}
            Self::BrowserPageFacts { key, .. } => push_opt(&mut out, key),
            Self::BrowserBeacon {
                url, request, key, ..
            } => {
                out.push(url);
                out.push(request);
                push_opt(&mut out, key);
            }
            Self::BrowserPrompt {
                text, schema, key, ..
            } => {
                out.push(text);
                out.push(schema);
                push_opt(&mut out, key);
            }
            Self::BrowserEmbed { texts, key, .. } => {
                out.push(texts);
                push_opt(&mut out, key);
            }
            Self::BrowserTab { request, key, .. } => {
                out.push(request);
                push_opt(&mut out, key);
            }
        }
        out
    }
}
