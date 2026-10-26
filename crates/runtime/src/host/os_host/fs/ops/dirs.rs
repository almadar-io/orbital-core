//! Directories: mkdir, rmdir, listing, glob walks and mktemp.

use super::*;

impl<'a> Cx<'a> {
    pub(super) fn mkdir(&self, args: &[Value]) -> Result<Value, OsError> {
        let at = self.locate(arg_str(args, 0, self.op)?, FsMode::Create, false)?;
        let recursive = match arg_opt(args, 1) {
            Some(_) => arg_bool(args, 1, self.op)?,
            None => false,
        };
        let mode = self.mode_arg(args, 2)?;
        if recursive {
            for ancestor in at.real.ancestors() {
                if fs::symlink_metadata(ancestor).is_err() {
                    self.allow(ancestor, FsMode::Create)?;
                }
            }
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(recursive);
        if let Some(mode) = mode {
            builder.mode(mode);
        }
        builder
            .create(&at.real)
            .map_err(|e| self.io(&at.real, &e))?;
        Ok(obj([]))
    }

    pub(super) fn require_tree_removable(&self, dir: &Path) -> Result<(), OsError> {
        for entry in fs::read_dir(dir).map_err(|e| self.io(dir, &e))? {
            let entry = entry.map_err(|e| self.io(dir, &e))?;
            let path = entry.path();
            self.allow(&path, FsMode::Remove)?;
            if entry.file_type().map_err(|e| self.io(&path, &e))?.is_dir() {
                self.require_tree_removable(&path)?;
            }
        }
        Ok(())
    }

    pub(super) fn rmdir(&self, args: &[Value]) -> Result<Value, OsError> {
        let path = self.path(arg_str(args, 0, self.op)?, FsMode::Remove, false)?;
        let recursive = match arg_opt(args, 1) {
            Some(_) => arg_bool(args, 1, self.op)?,
            None => false,
        };
        let md = fs::symlink_metadata(&path).map_err(|e| self.io(&path, &e))?;
        if !md.is_dir() {
            return Err(self.err(
                OsErrorKind::Invalid,
                format!("{} is not a directory; use os/remove", path.display()),
            ));
        }
        if recursive {
            self.require_tree_removable(&path)?;
            fs::remove_dir_all(&path)
        } else {
            fs::remove_dir(&path)
        }
        .map_err(|e| self.io(&path, &e))?;
        Ok(obj([]))
    }

    pub(super) fn list_dir(&self, args: &[Value]) -> Result<Value, OsError> {
        let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
        let mut entries: Vec<(String, &'static str)> = Vec::new();
        for entry in fs::read_dir(&path).map_err(|e| self.io(&path, &e))? {
            let entry = entry.map_err(|e| self.io(&path, &e))?;
            let file_type = entry.file_type().map_err(|e| self.io(&path, &e))?;
            entries.push((
                entry.file_name().to_string_lossy().into_owned(),
                kind_of(file_type),
            ));
        }
        entries.sort();
        let entries = entries
            .into_iter()
            .map(|(name, kind)| {
                obj([
                    ("name", Value::string(name)),
                    ("kind", Value::string(kind.to_string())),
                ])
            })
            .collect();
        Ok(obj([("entries", Value::Array(entries))]))
    }

    pub(super) fn glob(&self, args: &[Value]) -> Result<Value, OsError> {
        if let Some(anchored) = args.first().and_then(AnchoredPath::from_value) {
            return self.anchored_glob(&anchored);
        }
        let raw = arg_str(args, 0, self.op)?;
        if raw.is_empty() {
            return Err(self.err(OsErrorKind::Invalid, "empty pattern"));
        }
        let absolute = self.absolute(raw)?;
        let pattern = lexical(&absolute).ok_or_else(|| {
            self.err(OsErrorKind::Denied, format!("{raw} climbs above the root"))
        })?;
        let found = self.expand(&pattern, true)?;
        Ok(obj([(
            "paths",
            Value::Array(found.into_iter().map(Value::string).collect()),
        )]))
    }

    /// An anchored pattern matches under the boot-resolved anchor and answers anchor-relative paths. Its prefix
    /// need not be granted: a narrow anchored grant covers it, and every match is checked against the grants.
    pub(super) fn anchored_glob(&self, anchored: &AnchoredPath) -> Result<Value, OsError> {
        let anchors = self.anchors;
        if anchored.path.as_deref().is_none_or(str::is_empty) {
            return Err(self.err(OsErrorKind::Invalid, "empty pattern"));
        }
        if let GrantAnchor::Env(name) = &anchored.anchor {
            if anchors.env.get(name).is_none_or(str::is_empty) {
                return Err(self.err(OsErrorKind::NotFound, format!("env {name} is unset")));
            }
        }
        let denied = |why: String| self.err(OsErrorKind::Denied, why);
        let root = anchors
            .resolve_path(&AnchoredPath { anchor: anchored.anchor.clone(), path: None })
            .map_err(denied)?;
        let pattern = anchors.resolve_path(anchored).map_err(denied)?;
        let root = PathBuf::from(root);
        let found = self.expand(Path::new(&pattern), false)?;
        let relative = found
            .into_iter()
            .filter_map(|p| {
                let rel = Path::new(&p).strip_prefix(&root).ok()?.to_string_lossy().into_owned();
                Some(Value::string(if rel.is_empty() { ".".to_string() } else { rel }))
            })
            .collect();
        Ok(obj([("paths", Value::Array(relative))]))
    }

    pub(super) fn expand(&self, pattern: &Path, check_prefix: bool) -> Result<Vec<String>, OsError> {
        let mut prefix = PathBuf::from("/");
        let mut rest = 0usize;
        let mut wild = false;
        let mut deep = false;
        for component in pattern.components() {
            if let Component::Normal(part) = component {
                let part = part.to_string_lossy();
                if wild || part.contains(['*', '?']) {
                    wild = true;
                    rest += 1;
                    deep |= part == "**";
                } else {
                    prefix.push(&*part);
                }
            }
        }
        if check_prefix {
            self.allow(&prefix, FsMode::Read)?;
        }
        let matcher = OsGrants::read_matcher(&pattern.to_string_lossy());
        let mut found: Vec<String> = Vec::new();
        self.consider(&prefix, &matcher, &mut found);
        if wild {
            self.walk(
                &prefix,
                1,
                if deep { None } else { Some(rest) },
                &matcher,
                &mut found,
            );
        }
        found.sort();
        found.dedup();
        Ok(found)
    }

    pub(super) fn consider(&self, path: &Path, matcher: &OsGrants, out: &mut Vec<String>) {
        if !matcher.fs_allows(path, FsMode::Read)
            || !self.grants.fs_allows_at(path, FsMode::Read, self.anchors)
        {
            return;
        }
        let Ok(real) = resolve(path, true) else {
            return;
        };
        if real != path && !self.grants.fs_allows_at(&real, FsMode::Read, self.anchors) {
            return;
        }
        if fs::symlink_metadata(path).is_ok() {
            out.push(path.to_string_lossy().into_owned());
        }
    }

    pub(super) fn walk(
        &self,
        dir: &Path,
        level: usize,
        max_level: Option<usize>,
        matcher: &OsGrants,
        out: &mut Vec<String>,
    ) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut children: Vec<(PathBuf, bool)> = entries
            .flatten()
            .map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (e.path(), is_dir)
            })
            .collect();
        children.sort();
        for (path, is_dir) in children {
            self.consider(&path, matcher, out);
            if is_dir && max_level.is_none_or(|max| level < max) {
                self.walk(&path, level + 1, max_level, matcher, out);
            }
        }
    }

    pub(super) fn mktemp(&self, args: &[Value]) -> Result<Value, OsError> {
        let prefix = arg_str(args, 0, self.op)?;
        if prefix.contains('/') || prefix.contains('\0') || prefix == ".." || prefix == "." {
            return Err(self.err(
                OsErrorKind::Invalid,
                "prefix must be a plain file-name prefix",
            ));
        }
        let base = match arg_opt(args, 1) {
            Some(_) => arg_str(args, 1, self.op)?.to_string(),
            None => std::env::temp_dir().to_string_lossy().into_owned(),
        };
        let at = self.locate(&base, FsMode::Create, true)?;
        for _ in 0..64 {
            let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let mut x = nanos
                ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (u64::from(std::process::id()) << 32);
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^= x >> 31;
            let candidate = at.real.join(format!("{prefix}{x:016x}"));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
            {
                Ok(_) => return Ok(obj([("path", text(&candidate))])),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(self.io(&candidate, &e)),
            }
        }
        Err(self.err(OsErrorKind::Io, "could not find an unused temporary name"))
    }
}
