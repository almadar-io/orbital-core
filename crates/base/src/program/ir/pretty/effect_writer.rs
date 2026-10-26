//! Writing an effect.

use super::*;

impl PrettyPrinter {
    pub(super) fn write_effect(&mut self, effect: &OirEffect) {
        match effect {
            OirEffect::Set { target, value, .. } => {
                self.write("set(");
                self.write_binding(target);
                self.write(", ");
                self.write_expr(value);
                self.write(")");
            }
            OirEffect::SetDynamic { target, value, .. } => {
                self.write("set_dynamic(");
                self.write_expr(target);
                self.write(", ");
                self.write_expr(value);
                self.write(")");
            }
            OirEffect::Emit { event, payload } => {
                self.write(&format!("emit(\"{}\"", event));
                if let Some(p) = payload {
                    self.write(", ");
                    self.write_expr(p);
                }
                self.write(")");
            }
            OirEffect::Fetch {
                entity_type,
                id,
                filter,
                nearest,
                ..
            } => {
                self.write(&format!("fetch(\"{}\"", entity_type));
                if let Some(n) = nearest {
                    self.write(", nearest: ");
                    self.write_expr(n);
                }
                if let Some(id_expr) = id {
                    self.write(", id: ");
                    self.write_expr(id_expr);
                }
                if let Some(f) = filter {
                    self.write(", filter: ");
                    self.write_expr(f);
                }
                self.write(")");
            }
            OirEffect::Persist {
                operation,
                entity_type,
                data,
                ..
            } => {
                self.write(&format!("persist({:?}, \"{}\", ", operation, entity_type));
                self.write_expr(data);
                self.write(")");
            }
            OirEffect::PersistBatch { ops, .. } => {
                self.write("persist-batch(");
                self.write_expr(ops);
                self.write(")");
            }
            OirEffect::Ref {
                entity_type,
                id,
                filter,
                ..
            } => {
                self.write(&format!("ref(\"{}\"", entity_type));
                if let Some(id_expr) = id {
                    self.write(", id: ");
                    self.write_expr(id_expr);
                }
                if let Some(f) = filter {
                    self.write(", filter: ");
                    self.write_expr(f);
                }
                self.write(")");
            }
            OirEffect::Deref { entity_type, id } => {
                self.write(&format!("deref(\"{}\"", entity_type));
                if let Some(id_expr) = id {
                    self.write(", ");
                    self.write_expr(id_expr);
                }
                self.write(")");
            }
            OirEffect::Swap {
                entity_type,
                id,
                transform,
            } => {
                self.write(&format!("swap(\"{}\"", entity_type));
                self.write(", ");
                self.write_expr(id);
                self.write(", ");
                self.write_expr(transform);
                self.write(")");
            }
            OirEffect::Watch {
                entity_type,
                effects,
            } => {
                self.write(&format!("watch(\"{}\", [", entity_type));
                for (i, eff) in effects.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write_expr(eff);
                }
                self.write("])");
            }
            OirEffect::Atomic { effects } => {
                self.write("atomic([");
                for (i, eff) in effects.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write_expr(eff);
                }
                self.write("])");
            }
            OirEffect::Navigate { path, .. } => {
                self.write("navigate(");
                self.write_expr(path);
                self.write(")");
            }
            OirEffect::NavigateBack => {
                self.write("navigate-back()");
            }
            OirEffect::RenderUi { slot, pattern } => {
                self.write(&format!(
                    "render-ui(\"{}\", \"{}\")",
                    slot, pattern.pattern_type
                ));
            }
            OirEffect::CallService {
                service,
                method,
                args,
                ..
            } => {
                self.write(&format!("call-service(\"{}.{}\", ", service, method));
                self.write_expr(args);
                self.write(")");
            }
            OirEffect::Http {
                method,
                origin,
                path,
                request,
                ..
            } => {
                self.write(&format!("http({}, ", format!("{method:?}").to_uppercase()));
                self.write_expr(origin);
                self.write(", ");
                self.write_expr(path);
                if let Some(request) = request {
                    self.write(", ");
                    self.write_expr(request);
                }
                self.write(")");
            }
            OirEffect::Redis {
                origin,
                command,
                options,
                ..
            } => {
                self.write("redis(");
                self.write_expr(origin);
                self.write(", ");
                self.write_expr(command);
                if let Some(options) = options {
                    self.write(", ");
                    self.write_expr(options);
                }
                self.write(")");
            }
            OirEffect::Postgres {
                origin,
                sql,
                params,
                options,
                ..
            } => {
                self.write("postgres(");
                self.write_expr(origin);
                self.write(", ");
                self.write_expr(sql);
                self.write(", ");
                self.write_expr(params);
                if let Some(options) = options {
                    self.write(", ");
                    self.write_expr(options);
                }
                self.write(")");
            }
            OirEffect::CancelCall { key } => {
                self.write("cancel-call(");
                self.write_expr(key);
                self.write(")");
            }
            OirEffect::Os { op, args, .. } => {
                self.write(op.name());
                self.write("(");
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write_expr(arg);
                }
                self.write(")");
            }
            OirEffect::RandomBytes { bytes: expr, .. } => {
                self.write("random-bytes(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::CredentialStatus {
                credential: expr, ..
            } => {
                self.write("credential-status(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::CredentialForget {
                credential: expr, ..
            } => {
                self.write("credential-forget(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::CredentialAdmin { op, request, .. } => {
                self.write(op.name());
                self.write("(");
                self.write_expr(request);
                self.write(")");
            }
            OirEffect::AccessCheck { request, .. } => {
                self.write("access-check(");
                self.write_expr(request);
                self.write(")");
            }
            OirEffect::OAuthAuthorize {
                client, options, ..
            } => {
                self.write("oauth-authorize(");
                self.write_expr(client);
                self.write(", ");
                self.write_expr(options);
                self.write(")");
            }
            OirEffect::OAuthConnectionStatus { provider: expr, .. } => {
                self.write("oauth-connection-status(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::OAuthConnectionForget { provider: expr, .. } => {
                self.write("oauth-connection-forget(");
                self.write_expr(expr);
                self.write(")");
            }
            OirEffect::Spawn { entity_type, props } => {
                self.write(&format!("spawn(\"{}\", ", entity_type));
                self.write_expr(props);
                self.write(")");
            }
            OirEffect::Despawn { entity_id } => {
                self.write("despawn(");
                self.write_expr(entity_id);
                self.write(")");
            }
            OirEffect::Log { level, message } => {
                self.write(&format!("log({:?}, ", level));
                self.write_expr(message);
                self.write(")");
            }
            OirEffect::Custom { name, args, .. } => {
                self.write(&format!("custom(\"{}\"", name));
                for (k, v) in args {
                    self.write(&format!(", {}: ", k));
                    self.write_expr(v);
                }
                self.write(")");
            }
            OirEffect::OsWatchFiles { glob, options, .. } => {
                self.write("os/watch-files(");
                self.write_expr(glob);
                if let Some(o) = options {
                    self.write(", ");
                    self.write_expr(o);
                }
                self.write(")");
            }
            OirEffect::OsWatchProcess {
                name, subcommand, ..
            } => {
                self.write("os/watch-process(");
                self.write_expr(name);
                if let Some(s) = subcommand {
                    self.write(", ");
                    self.write_expr(s);
                }
                self.write(")");
            }
            OirEffect::OsWatchPort { port, protocol, .. } => {
                self.write("os/watch-port(");
                self.write_expr(port);
                if let Some(p) = protocol {
                    self.write(", ");
                    self.write_expr(p);
                }
                self.write(")");
            }
            OirEffect::OsWatchHttp {
                url_pattern,
                method,
                ..
            } => {
                self.write("os/watch-http(");
                self.write_expr(url_pattern);
                if let Some(m) = method {
                    self.write(", ");
                    self.write_expr(m);
                }
                self.write(")");
            }
            OirEffect::OsWatchCron { expression, .. } => {
                self.write("os/watch-cron(");
                self.write_expr(expression);
                self.write(")");
            }
            OirEffect::OsWatchSignal { signal, .. } => {
                self.write("os/watch-signal(");
                self.write_expr(signal);
                self.write(")");
            }
            OirEffect::OsWatchEnv { variable, .. } => {
                self.write("os/watch-env(");
                self.write_expr(variable);
                self.write(")");
            }
            OirEffect::FetchStream {
                entity_type,
                id,
                filter,
                ..
            } => {
                self.write(&format!("fetch-stream({}", entity_type));
                if let Some(id_expr) = id {
                    self.write(", id=");
                    self.write_expr(id_expr);
                }
                if let Some(filter_expr) = filter {
                    self.write(", filter=");
                    self.write_expr(filter_expr);
                }
                self.write(")");
            }
            OirEffect::SendServer { event, payload } => {
                self.write("send-server(event=");
                self.write_expr(event);
                if let Some(p) = payload {
                    self.write(", payload=");
                    self.write_expr(p);
                }
                self.write(")");
            }
            OirEffect::BrowserOpenFilePicker { options, .. } => {
                self.write("browser/open-file-picker(");
                if let Some(o) = options {
                    self.write_expr(o);
                }
                self.write(")");
            }
            OirEffect::BrowserClipboardRead { .. } => {
                self.write("browser/clipboard-read()");
            }
            OirEffect::BrowserClipboardWrite { text, .. } => {
                self.write("browser/clipboard-write(");
                self.write_expr(text);
                self.write(")");
            }
            OirEffect::BrowserGeolocationCurrent { options, .. } => {
                self.write("browser/geolocation-current(");
                if let Some(o) = options {
                    self.write_expr(o);
                }
                self.write(")");
            }
            OirEffect::BrowserPushSubscribe { .. } => {
                self.write("browser/push-subscribe()");
            }
            OirEffect::BrowserPageFacts { fields, .. } => {
                let words: Vec<&str> = fields.iter().map(|f| f.as_str()).collect();
                self.write(&format!("browser/page-facts([{}])", words.join(", ")));
            }
            OirEffect::BrowserPrompt {
                text, schema, key, ..
            } => {
                self.write("browser/prompt(");
                self.write_expr(text);
                self.write(", ");
                self.write_expr(schema);
                if let Some(k) = key {
                    self.write(", ");
                    self.write_expr(k);
                }
                self.write(")");
            }
            OirEffect::BrowserEmbed { texts, key, .. } => {
                self.write("browser/embed(");
                self.write_expr(texts);
                if let Some(k) = key {
                    self.write(", ");
                    self.write_expr(k);
                }
                self.write(")");
            }
            OirEffect::BrowserTab { op, request, key, .. } => {
                self.write(op.name());
                self.write("(");
                self.write_expr(request);
                if let Some(k) = key {
                    self.write(", ");
                    self.write_expr(k);
                }
                self.write(")");
            }
            OirEffect::BrowserBeacon {
                url, request, key, ..
            } => {
                self.write("browser/beacon(");
                self.write_expr(url);
                self.write(", ");
                self.write_expr(request);
                if let Some(k) = key {
                    self.write(", ");
                    self.write_expr(k);
                }
                self.write(")");
            }
        }
    }
}
