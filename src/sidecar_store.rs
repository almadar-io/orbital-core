//! Resolution of lock-declared behavior-package paths into the content-addressed
//! sidecar cache. The network is injected (`SidecarFetcher`) so this crate stays
//! network-free; the rule is the one in `docs/Almadar_Behavior_Packages.md` §13:
//! a lock entry always resolves to `<cache>/<sha256>`, downloaded and verified on a miss.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::behavior_packages::{url_host, LockedSidecar};

/// Streams the body behind `url` into `sink`. The returned error text must not contain `url`.
pub trait SidecarFetcher {
    fn fetch(&self, url: &str, sink: &mut dyn Write) -> Result<(), String>;
}

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("ORB_SIDECAR_FETCH_FAILED: {package}@{version} {path} (sha256 {sha256}) from {host} — {reason}")]
    FetchFailed {
        package: String,
        version: String,
        path: String,
        sha256: String,
        host: String,
        reason: String,
    },
    #[error("ORB_SIDECAR_HASH_MISMATCH: {package}@{version} {path} — expected sha256 {sha256} ({bytes} bytes), {detail}")]
    HashMismatch {
        package: String,
        version: String,
        path: String,
        sha256: String,
        bytes: u64,
        detail: String,
    },
}

struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Returns the cache path of `sidecar`, downloading and verifying it first when absent.
/// A bad cached file or a bad download never stays in the cache.
#[expect(clippy::result_large_err, reason = "public unboxed layout is the API; pinned by tests/public_layout_contract.rs")]
pub fn ensure_sidecar(
    package: &str,
    version: &str,
    sidecar: &LockedSidecar,
    fetcher: &dyn SidecarFetcher,
) -> Result<PathBuf, SidecarError> {
    let mismatch = |detail: String| SidecarError::HashMismatch {
        package: package.to_string(),
        version: version.to_string(),
        path: sidecar.rel_path.clone(),
        sha256: sidecar.sha256.clone(),
        bytes: sidecar.bytes,
        detail,
    };
    let fetch_failed = |reason: String| SidecarError::FetchFailed {
        package: package.to_string(),
        version: version.to_string(),
        path: sidecar.rel_path.clone(),
        sha256: sidecar.sha256.clone(),
        host: url_host(&sidecar.url).unwrap_or("unknown host").to_string(),
        reason: reason.replace(sidecar.url.as_str(), "<redacted url>"),
    };

    let cached = &sidecar.cache_path;
    if let Ok(meta) = fs::metadata(cached) {
        if meta.len() == sidecar.bytes {
            return Ok(cached.clone());
        }
        let _ = fs::remove_file(cached);
        return Err(mismatch(format!("cached file has {} bytes", meta.len())));
    }

    let dir = cached
        .parent()
        .ok_or_else(|| fetch_failed("cache path has no parent directory".to_string()))?;
    fs::create_dir_all(dir)
        .map_err(|e| fetch_failed(format!("cannot create cache directory: {e}")))?;
    let temp = temp_path(cached);
    let file =
        fs::File::create(&temp).map_err(|e| fetch_failed(format!("cannot write cache: {e}")))?;
    let mut sink = HashingWriter {
        inner: io::BufWriter::new(file),
        hasher: Sha256::new(),
        written: 0,
    };
    let outcome = fetcher
        .fetch(&sidecar.url, &mut sink)
        .map_err(&fetch_failed)
        .and_then(|()| {
            sink.flush()
                .map_err(|e| fetch_failed(format!("cannot write cache: {e}")))
        });
    let HashingWriter {
        inner,
        hasher,
        written,
    } = sink;
    drop(inner);
    let actual = hex(&hasher.finalize());
    if let Err(e) = outcome {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    if actual != sidecar.sha256 || written != sidecar.bytes {
        let _ = fs::remove_file(&temp);
        return Err(mismatch(format!(
            "downloaded {written} bytes with sha256 {actual}"
        )));
    }
    fs::rename(&temp, cached).map_err(|e| {
        let _ = fs::remove_file(&temp);
        fetch_failed(format!("cannot publish cache file: {e}"))
    })?;
    Ok(cached.clone())
}

fn temp_path(cached: &Path) -> PathBuf {
    let mut name = cached.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    cached.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "orbital-core-sidecar-store-{}-{}",
                std::process::id(),
                tag
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const TOKEN: &str = "SECRETTOKEN123";

    fn url() -> String {
        format!("https://storage.example.test/v0/b/x/o/sidecars%2Fabc?alt=media&token={TOKEN}")
    }

    fn sidecar(cache: &Path, body: &[u8]) -> LockedSidecar {
        let sha256 = hex(&Sha256::digest(body));
        LockedSidecar {
            rel_path: "dist/knob-embeddings.json".to_string(),
            cache_path: cache.join(&sha256),
            sha256,
            bytes: body.len() as u64,
            url: url(),
        }
    }

    struct Fake {
        body: Vec<u8>,
        error: Option<String>,
        calls: Cell<u32>,
    }
    impl Fake {
        fn ok(body: &[u8]) -> Self {
            Fake {
                body: body.to_vec(),
                error: None,
                calls: Cell::new(0),
            }
        }
        fn failing(error: &str) -> Self {
            Fake {
                body: b"partial".to_vec(),
                error: Some(error.to_string()),
                calls: Cell::new(0),
            }
        }
    }
    impl SidecarFetcher for Fake {
        fn fetch(&self, _url: &str, sink: &mut dyn Write) -> Result<(), String> {
            self.calls.set(self.calls.get() + 1);
            sink.write_all(&self.body).map_err(|e| e.to_string())?;
            match &self.error {
                Some(e) => Err(e.clone()),
                None => Ok(()),
            }
        }
    }

    fn cache_files(cache: &Path) -> Vec<String> {
        match fs::read_dir(cache) {
            Ok(rd) => rd
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    #[test]
    fn cache_hit_does_not_fetch() {
        let tmp = TempDir::new("hit");
        let body = b"hello vectors";
        let s = sidecar(&tmp.0, body);
        fs::write(&s.cache_path, body).unwrap();
        let fake = Fake::ok(b"WRONG");
        assert_eq!(
            ensure_sidecar("p", "1.0.0", &s, &fake).unwrap(),
            s.cache_path
        );
        assert_eq!(fake.calls.get(), 0);
        assert_eq!(fs::read(&s.cache_path).unwrap(), body);
    }

    #[test]
    fn miss_fetches_verifies_and_stores_atomically() {
        let tmp = TempDir::new("miss");
        let cache = tmp.0.join("nested/cache");
        let body = b"hello vectors";
        let s = sidecar(&cache, body);
        let fake = Fake::ok(body);
        let path = ensure_sidecar("p", "1.0.0", &s, &fake).unwrap();
        assert_eq!(fake.calls.get(), 1);
        assert_eq!(fs::read(path).unwrap(), body);
        assert_eq!(cache_files(&cache), vec![s.sha256.clone()]);
    }

    #[test]
    fn second_call_after_miss_is_a_hit() {
        let tmp = TempDir::new("twice");
        let body = b"abc";
        let s = sidecar(&tmp.0, body);
        let fake = Fake::ok(body);
        ensure_sidecar("p", "1.0.0", &s, &fake).unwrap();
        ensure_sidecar("p", "1.0.0", &s, &fake).unwrap();
        assert_eq!(fake.calls.get(), 1);
    }

    #[test]
    fn downloaded_bytes_with_wrong_hash_error_and_leave_no_file() {
        let tmp = TempDir::new("badhash");
        let s = sidecar(&tmp.0, b"hello vectors");
        let fake = Fake::ok(b"HELLO VECTORS");
        let err = ensure_sidecar("p", "1.0.0", &s, &fake).unwrap_err();
        assert!(
            err.to_string().starts_with("ORB_SIDECAR_HASH_MISMATCH"),
            "{err}"
        );
        assert!(err.to_string().contains(&s.sha256));
        assert!(cache_files(&tmp.0).is_empty(), "{:?}", cache_files(&tmp.0));
    }

    #[test]
    fn downloaded_bytes_with_wrong_length_error_and_leave_no_file() {
        let tmp = TempDir::new("badlen");
        let mut s = sidecar(&tmp.0, b"abc");
        s.bytes = 99;
        let err = ensure_sidecar("p", "1.0.0", &s, &Fake::ok(b"abc")).unwrap_err();
        assert!(
            err.to_string().starts_with("ORB_SIDECAR_HASH_MISMATCH"),
            "{err}"
        );
        assert!(cache_files(&tmp.0).is_empty());
    }

    #[test]
    fn cached_file_of_wrong_size_errors_without_fetch_and_is_deleted() {
        let tmp = TempDir::new("badcached");
        let s = sidecar(&tmp.0, b"hello vectors");
        fs::write(&s.cache_path, b"trunc").unwrap();
        let fake = Fake::ok(b"hello vectors");
        let err = ensure_sidecar("p", "1.0.0", &s, &fake).unwrap_err();
        assert!(
            err.to_string().starts_with("ORB_SIDECAR_HASH_MISMATCH"),
            "{err}"
        );
        assert_eq!(fake.calls.get(), 0);
        assert!(cache_files(&tmp.0).is_empty());
    }

    #[test]
    fn fetch_failure_names_path_sha_and_host_but_never_the_token() {
        let tmp = TempDir::new("fail");
        let s = sidecar(&tmp.0, b"hello vectors");
        let leaky = format!("GET {} failed: 403", s.url);
        let fake = Fake::failing(&leaky);
        let msg = ensure_sidecar("pkg", "2.0.0", &s, &fake)
            .unwrap_err()
            .to_string();
        assert!(msg.starts_with("ORB_SIDECAR_FETCH_FAILED"), "{msg}");
        assert!(msg.contains("dist/knob-embeddings.json"));
        assert!(msg.contains(&s.sha256));
        assert!(msg.contains("storage.example.test"));
        assert!(!msg.contains(TOKEN), "{msg}");
        assert!(!msg.contains("alt=media"), "{msg}");
        assert!(
            cache_files(&tmp.0).is_empty(),
            "partial write must be removed"
        );
    }

    #[test]
    fn url_host_drops_path_query_and_userinfo() {
        assert_eq!(url_host("https://a.b:8443/x?token=1"), Some("a.b:8443"));
        assert_eq!(url_host("https://user:pw@h.test/x"), Some("h.test"));
        assert_eq!(url_host("not a url"), None);
        assert_eq!(url_host("https:///x"), None);
    }
}
