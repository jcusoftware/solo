//! Native-only local filesystem asset store for offline/local use.
//!
//! This module is compiled out of `wasm32` entirely (`#[cfg(not(target_arch = "wasm32"))]`
//! in `lib.rs`): the Worker never touches a filesystem. It enforces the same limits
//! and the same reserve, stream-count, verify, commit algorithm as the D1 and R2
//! backends (see [`crate::quota`]).
//!
//! Layout under `root` (one store per workspace):
//!
//! - `<site>/<sha256>.<ext>`: committed assets.
//! - `<site>/.up-<pid>-<nanos>-<n>.tmp`: in-flight uploads, created with `create_new`
//!   so an upload can never clobber another's file.
//! - `.res/<id>.res`: live reservations (`site bytes expires_at`), on disk so they are
//!   visible to every process and store handle on the same root.
//! - `.lock`: exclusive advisory lock (`File::lock`) held around reserve, commit,
//!   delete and reconcile. Streaming a body does NOT hold it, so a slow upload never
//!   blocks other reservers. Because all state is on disk under that lock, separate
//!   processes or store handles can never over-commit.
//!
//! The commit re-reads the clock and refuses (`reservation_expired`) when the
//! reservation has lapsed, exactly like the D1/R2 commit statement, so bytes are never
//! committed after the quota stopped counting them. Reconciliation only removes
//! temp files whose mtime is at least [`RESERVATION_TTL_SECS`] old, so it can never
//! remove a live upload's file. `get` re-hashes the file and fails closed on tampering.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AssetError;
use crate::hash::{StreamHasher, sha256_hex};
use crate::limits::{AssetLimits, RESERVATION_TTL_SECS};
use crate::quota::{StreamCounter, Usage, diagnose, fits, plan_upload};
use crate::validate::{
    ContentKind, check_declared_vs_sniffed, declared_kind, is_id, is_sha256, local_file_name,
};

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Unique per process and per call: pid + nanoseconds + counter.
fn unique_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

struct LockGuard(fs::File);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reservation {
    id: String,
    site: String,
    bytes: u64,
    expires_at: u64,
}

#[derive(Clone)]
pub struct LocalAssetStore {
    root: PathBuf,
    limits: AssetLimits,
    clock: Clock,
}

impl std::fmt::Debug for LocalAssetStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalAssetStore")
            .field("root", &self.root)
            .field("limits", &self.limits)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub sha256: String,
    pub byte_size: u64,
    pub kind: ContentKind,
    /// False when identical bytes were already stored (no extra space used).
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub sha256: String,
    pub byte_size: u64,
    pub kind: ContentKind,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub expired_reservations: usize,
    pub tmp_files_removed: usize,
}

fn io(_: std::io::Error) -> AssetError {
    AssetError::StoreFailed
}

impl LocalAssetStore {
    pub fn open(root: impl Into<PathBuf>, limits: AssetLimits) -> Result<Self, AssetError> {
        let root = root.into();
        fs::create_dir_all(root.join(".res")).map_err(io)?;
        Ok(Self {
            root,
            limits,
            clock: Arc::new(system_now),
        })
    }

    /// Replace the commit-time clock (tests; the default is the system clock).
    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    fn site_dir(&self, site: &str) -> Result<PathBuf, AssetError> {
        if is_id(site) {
            Ok(self.root.join(site))
        } else {
            Err(AssetError::SiteNotFound)
        }
    }

    fn lock(&self) -> Result<LockGuard, AssetError> {
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".lock"))
            .map_err(io)?;
        f.lock().map_err(io)?;
        Ok(LockGuard(f))
    }

    fn res_dir(&self) -> PathBuf {
        self.root.join(".res")
    }

    fn read_reservations(&self) -> Vec<(PathBuf, Reservation)> {
        let Ok(rd) = fs::read_dir(self.res_dir()) else {
            return vec![];
        };
        rd.flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let id = name.strip_suffix(".res")?.to_string();
                let text = fs::read_to_string(e.path()).ok()?;
                let mut it = text.split_whitespace();
                let site = it.next()?.to_string();
                let bytes = it.next()?.parse().ok()?;
                let expires_at = it.next()?.parse().ok()?;
                Some((
                    e.path(),
                    Reservation {
                        id,
                        site,
                        bytes,
                        expires_at,
                    },
                ))
            })
            .collect()
    }

    /// Committed files of one site: `(sha256, size, kind)`.
    fn committed(dir: &Path) -> Vec<Entry> {
        let Ok(rd) = fs::read_dir(dir) else {
            return vec![];
        };
        rd.flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let (sha, ext) = name.split_once('.')?;
                let kind = ContentKind::from_ext(ext)?;
                is_sha256(sha).then_some(())?;
                Some(Entry {
                    sha256: sha.to_string(),
                    byte_size: e.metadata().ok()?.len(),
                    kind,
                })
            })
            .collect()
    }

    /// Caller holds the lock.
    fn usage_locked(&self, site: &str, now: u64) -> Usage {
        let mut u = Usage::default();
        if let Ok(rd) = fs::read_dir(&self.root) {
            for d in rd.flatten() {
                let Some(name) = d.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !is_id(&name) {
                    continue;
                }
                let files = Self::committed(&d.path());
                let bytes: u64 = files.iter().map(|f| f.byte_size).sum();
                u.workspace_bytes += bytes;
                if name == site {
                    u.site_bytes += bytes;
                    u.site_assets += files.len() as u64;
                }
            }
        }
        for (_, r) in self
            .read_reservations()
            .iter()
            .filter(|(_, r)| r.expires_at > now)
        {
            u.workspace_bytes += r.bytes;
            if r.site == site {
                u.site_bytes += r.bytes;
                u.site_assets += 1;
            }
        }
        u
    }

    pub fn usage(&self, site: &str, now: u64) -> Usage {
        match self.lock() {
            Ok(_g) => self.usage_locked(site, now),
            Err(_) => Usage::default(),
        }
    }

    fn reserve(&self, site: &str, bytes: u64, now: u64) -> Result<String, AssetError> {
        let _g = self.lock()?;
        let usage = self.usage_locked(site, now);
        if !fits(&usage, bytes, &self.limits) {
            return Err(diagnose(&usage, bytes, &self.limits));
        }
        let id = unique_id();
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.res_dir().join(format!("{id}.res")))
            .map_err(io)?;
        write!(f, "{site} {bytes} {}", now + RESERVATION_TTL_SECS).map_err(io)?;
        Ok(id)
    }

    fn release(&self, id: &str) {
        if let Ok(_g) = self.lock() {
            let _ = fs::remove_file(self.res_dir().join(format!("{id}.res")));
        }
    }

    /// Store `reader`'s bytes under `site`, content-addressed. `expected_sha256`
    /// (when given) must match the received bytes. `declared_len` is the optional
    /// `Content-Length`-style hint; the bytes actually read are what is enforced.
    /// `now` is the request-start time (reservation creation); commit re-reads the
    /// store clock.
    pub fn put(
        &self,
        site: &str,
        content_type: Option<&str>,
        expected_sha256: Option<&str>,
        declared_len: Option<&str>,
        reader: &mut dyn Read,
        now: u64,
    ) -> Result<Stored, AssetError> {
        let dir = self.site_dir(site)?;
        let kind = declared_kind(content_type)?;
        if expected_sha256.is_some_and(|s| !is_sha256(s)) {
            return Err(AssetError::InvalidSha256);
        }
        let plan = plan_upload(declared_len, &self.limits)?;
        let rid = self.reserve(site, plan.reserve_bytes, now)?;
        let result = self.receive(&dir, kind, expected_sha256, plan, reader, &rid);
        self.release(&rid);
        result
    }

    fn receive(
        &self,
        dir: &Path,
        kind: ContentKind,
        expected: Option<&str>,
        plan: crate::quota::UploadPlan,
        reader: &mut dyn Read,
        rid: &str,
    ) -> Result<Stored, AssetError> {
        fs::create_dir_all(dir).map_err(io)?;
        let tmp = dir.join(format!(".up-{rid}.tmp"));
        // create_new: never open (or later delete) a file we did not create.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(io)?;
        let outcome = (|| {
            let mut counter = StreamCounter::new(plan);
            let mut hasher = StreamHasher::new();
            let mut head: Vec<u8> = Vec::with_capacity(16);
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = reader.read(&mut buf).map_err(io)?;
                if n == 0 {
                    break;
                }
                counter.push(n)?; // reject over-cap mid-stream, before writing more
                if head.len() < 16 {
                    let take = (16 - head.len()).min(n);
                    head.extend_from_slice(&buf[..take]);
                }
                hasher.update(&buf[..n]);
                file.write_all(&buf[..n]).map_err(io)?;
            }
            file.flush().map_err(io)?;
            let size = counter.finish()?;
            check_declared_vs_sniffed(kind, &head)?;
            let sha = hasher.finish_hex();
            if expected.is_some_and(|e| e != sha) {
                return Err(AssetError::Sha256Mismatch);
            }
            Ok((sha, size))
        })();
        let (sha, size) = match outcome {
            Ok(v) => v,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        };
        let name = local_file_name(&sha, kind).ok_or(AssetError::InvalidSha256)?;
        let dest = dir.join(name);
        // Commit under the lock, against a clock read NOW (not the request start).
        let committed = (|| {
            let _g = self.lock()?;
            let res_path = self.res_dir().join(format!("{rid}.res"));
            let alive = self
                .read_reservations()
                .iter()
                .any(|(_, r)| r.id == rid && r.expires_at > (self.clock)());
            if !alive {
                return Err(AssetError::ReservationExpired);
            }
            let created = !dest.exists();
            if created {
                fs::rename(&tmp, &dest).map_err(io)?;
            }
            let _ = fs::remove_file(res_path);
            Ok(created)
        })();
        match committed {
            Ok(created) => {
                if !created {
                    let _ = fs::remove_file(&tmp);
                }
                Ok(Stored {
                    sha256: sha,
                    byte_size: size,
                    kind,
                    created,
                })
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    fn find(&self, site: &str, sha256: &str) -> Result<(PathBuf, ContentKind), AssetError> {
        if !is_sha256(sha256) {
            return Err(AssetError::InvalidSha256);
        }
        let dir = self.site_dir(site)?;
        for kind in ContentKind::ALL {
            if let Some(name) = local_file_name(sha256, kind) {
                let p = dir.join(name);
                if p.is_file() {
                    return Ok((p, kind));
                }
            }
        }
        Err(AssetError::AssetNotFound)
    }

    /// Read an asset, re-verifying its SHA-256: a file changed on disk since it was
    /// stored fails closed with `store_failed`.
    pub fn get(&self, site: &str, sha256: &str) -> Result<(ContentKind, Vec<u8>), AssetError> {
        let (p, kind) = self.find(site, sha256)?;
        let bytes = fs::read(p).map_err(io)?;
        if sha256_hex(&bytes) != sha256 {
            return Err(AssetError::StoreFailed);
        }
        Ok((kind, bytes))
    }

    pub fn delete(&self, site: &str, sha256: &str) -> Result<(), AssetError> {
        let _guard = self.lock()?;
        let (p, _) = self.find(site, sha256)?;
        fs::remove_file(p).map_err(io)
    }

    pub fn list(&self, site: &str) -> Result<Vec<Entry>, AssetError> {
        let mut v = Self::committed(&self.site_dir(site)?);
        v.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        Ok(v)
    }

    /// Drop expired reservations and temp files older than [`RESERVATION_TTL_SECS`]
    /// by mtime. A younger temp file may belong to a live upload and is never touched.
    pub fn reconcile(&self, now: u64) -> ReconcileReport {
        let mut report = ReconcileReport::default();
        let Ok(_g) = self.lock() else {
            return report;
        };
        for (path, r) in self.read_reservations() {
            if r.expires_at <= now && fs::remove_file(path).is_ok() {
                report.expired_reservations += 1;
            }
        }
        let Ok(rd) = fs::read_dir(&self.root) else {
            return report;
        };
        for d in rd.flatten() {
            if !d.file_name().to_str().is_some_and(is_id) {
                continue;
            }
            let Ok(files) = fs::read_dir(d.path()) else {
                continue;
            };
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if !(name.starts_with(".up-") && name.ends_with(".tmp")) {
                    continue;
                }
                let mtime = f
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                // Unknown age is treated as young (kept).
                let old = mtime.is_some_and(|m| now.saturating_sub(m) >= RESERVATION_TTL_SECS);
                if old && fs::remove_file(f.path()).is_ok() {
                    report.tmp_files_removed += 1;
                }
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{CARDLESS_D1, KIB};
    use std::io::Cursor;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    const PNG: Option<&str> = Some("image/png");
    const NOW: u64 = 1_795_737_600;

    fn png(len: usize, salt: u8) -> Vec<u8> {
        let mut v = PNG_SIG.to_vec();
        v.resize(len, salt);
        v[8] = salt;
        v
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "sws-assets-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn small() -> AssetLimits {
        AssetLimits {
            max_asset_bytes: 2 * KIB,
            max_site_bytes: 10 * KIB,
            max_workspace_bytes: 12 * KIB,
            max_site_assets: 100,
            max_database_bytes: u64::MAX,
        }
    }

    fn store(dir: &Path, limits: AssetLimits) -> LocalAssetStore {
        LocalAssetStore::open(dir, limits)
            .unwrap()
            .with_clock(|| NOW)
    }

    fn put(
        s: &LocalAssetStore,
        site: &str,
        data: &[u8],
        len: Option<&str>,
    ) -> Result<Stored, AssetError> {
        s.put(site, PNG, None, len, &mut Cursor::new(data.to_vec()), NOW)
    }

    #[test]
    fn round_trip_dedupe_and_delete() {
        let dir = tmpdir("rt");
        let s = store(&dir, CARDLESS_D1);
        let data = png(1000, 1);
        let st = put(&s, "site-a", &data, Some("1000")).unwrap();
        assert!(st.created);
        assert_eq!(st.sha256, sha256_hex(&data));
        assert!(!put(&s, "site-a", &data, None).unwrap().created);
        assert_eq!(
            s.get("site-a", &st.sha256).unwrap(),
            (ContentKind::Png, data)
        );
        assert_eq!(s.list("site-a").unwrap().len(), 1);
        s.delete("site-a", &st.sha256).unwrap();
        assert_eq!(s.get("site-a", &st.sha256), Err(AssetError::AssetNotFound));
        assert_eq!(s.usage("site-a", NOW), Usage::default());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn enforces_actual_bytes_and_cleans_up_failures() {
        let dir = tmpdir("bounds");
        let s = store(&dir, small());
        // No Content-Length: the stream itself is cut at the cap.
        assert_eq!(
            put(&s, "s", &png(2 * 1024 + 1, 1), None),
            Err(AssetError::AssetTooLarge)
        );
        // Runtime truncation: the body is longer than the declared Content-Length.
        assert_eq!(
            put(&s, "s", &png(500, 1), Some("100")),
            Err(AssetError::ContentLengthMismatch)
        );
        // Declared over cap is refused before reading.
        assert_eq!(
            put(&s, "s", &png(10, 1), Some("999999")),
            Err(AssetError::AssetTooLarge)
        );
        assert_eq!(put(&s, "s", b"", None), Err(AssetError::EmptyAsset));
        assert_eq!(
            put(&s, "s", b"<script>alert(1)</script>", None),
            Err(AssetError::ContentTypeMismatch)
        );
        assert_eq!(
            s.put(
                "s",
                Some("image/svg+xml"),
                None,
                None,
                &mut Cursor::new(vec![1]),
                NOW
            ),
            Err(AssetError::UnsupportedContentType)
        );
        assert_eq!(
            s.put(
                "s",
                PNG,
                Some(&"0".repeat(64)),
                None,
                &mut Cursor::new(png(100, 1)),
                NOW
            ),
            Err(AssetError::Sha256Mismatch)
        );
        assert_eq!(
            put(&s, "../x", &png(10, 1), None),
            Err(AssetError::SiteNotFound)
        );
        // Nothing leaked: no reservations, no tmp files, zero usage.
        assert_eq!(s.usage("s", NOW), Usage::default());
        assert_eq!(s.reconcile(NOW), ReconcileReport::default());
        let leftovers = fs::read_dir(dir.join("s")).map(|r| r.count()).unwrap_or(0);
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn site_and_workspace_quotas() {
        let dir = tmpdir("quota");
        let s = store(&dir, small());
        for i in 0..5u8 {
            put(&s, "a", &png(2 * 1024, i), None).unwrap();
        }
        assert_eq!(
            put(&s, "a", &png(2 * 1024, 99), None),
            Err(AssetError::SiteQuotaExceeded)
        );
        // Other site still has room, until the workspace cap (12 KiB) bites.
        put(&s, "b", &png(2 * 1024, 50), None).unwrap();
        assert_eq!(
            put(&s, "b", &png(2 * 1024, 51), None),
            Err(AssetError::WorkspaceQuotaExceeded)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn parallel_uploads_never_over_commit() {
        let dir = tmpdir("par");
        let s = Arc::new(store(&dir, small()));
        let handles: Vec<_> = (0..24u8)
            .map(|i| {
                let s = Arc::clone(&s);
                std::thread::spawn(move || put(&s, "a", &png(2 * 1024, i), None))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let ok = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok, 5, "exactly 10 KiB / 2 KiB fit: {results:?}");
        assert!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .all(|e| *e == AssetError::SiteQuotaExceeded)
        );
        let u = s.usage("a", NOW);
        assert_eq!(u.site_bytes, 10 * KIB);
        assert_eq!(s.reconcile(NOW), ReconcileReport::default());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reconcile_sweeps_stale_reservations_and_tmp_files() {
        let dir = tmpdir("rec");
        let s = store(&dir, small());
        s.reserve("a", 2 * KIB, NOW).unwrap();
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a").join(".up-young.tmp"), b"partial").unwrap();
        let old = dir.join("a").join(".up-old.tmp");
        fs::write(&old, b"orphan").unwrap();
        set_mtime(&old, NOW - RESERVATION_TTL_SECS - 1);
        set_mtime(&dir.join("a").join(".up-young.tmp"), NOW);
        // Only the temp file older than the TTL is removed; the young one may be live.
        assert_eq!(
            s.reconcile(NOW),
            ReconcileReport {
                expired_reservations: 0,
                tmp_files_removed: 1
            }
        );
        assert!(dir.join("a").join(".up-young.tmp").exists());
        assert_eq!(s.usage("a", NOW).site_bytes, 2 * KIB);
        // Past the TTL the reservation stops counting even before the sweep ...
        let later = NOW + RESERVATION_TTL_SECS + 1;
        assert_eq!(s.usage("a", later), Usage::default());
        // ... and the sweep removes it and its tmp file.
        assert_eq!(
            s.reconcile(later),
            ReconcileReport {
                expired_reservations: 1,
                tmp_files_removed: 1
            }
        );
        assert!(!dir.join("a").join(".up-young.tmp").exists());
        let _ = fs::remove_dir_all(dir);
    }

    fn set_mtime(p: &Path, secs: u64) {
        let f = fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn independent_handles_on_one_root_never_over_commit() {
        let dir = tmpdir("multi");
        let _ = store(&dir, small()); // create the root
        let handles: Vec<_> = (0..24u8)
            .map(|i| {
                let d = dir.clone();
                // Each thread opens its OWN store: no shared in-memory state at all.
                std::thread::spawn(move || {
                    let s = store(&d, small());
                    put(&s, "a", &png(2 * 1024, i), None)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            5,
            "{results:?}"
        );
        let s = store(&dir, small());
        assert_eq!(s.usage("a", NOW).site_bytes, 10 * KIB);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_lock_is_exclusive_while_held() {
        let dir = tmpdir("lock");
        let s = store(&dir, small());
        let g = s.lock().unwrap();
        let other = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(".lock"))
            .unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(g);
        assert!(other.try_lock().is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn temp_files_are_created_exclusively_and_uniquely() {
        let dir = tmpdir("excl");
        let s = store(&dir, small());
        let a = unique_id();
        let b = unique_id();
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("{}-", std::process::id())));
        // A pre-existing file at the temp path is never clobbered (create_new).
        fs::create_dir_all(dir.join("s")).unwrap();
        let rid = "fixed-id";
        let tmp = dir.join("s").join(format!(".up-{rid}.tmp"));
        fs::write(&tmp, b"victim").unwrap();
        let plan = plan_upload(Some("100"), &small()).unwrap();
        let r = s.receive(
            &dir.join("s"),
            ContentKind::Png,
            None,
            plan,
            &mut Cursor::new(png(100, 1)),
            rid,
        );
        assert_eq!(r, Err(AssetError::StoreFailed));
        assert_eq!(fs::read(&tmp).unwrap(), b"victim");
        let _ = fs::remove_dir_all(dir);
    }

    struct SlowReader {
        data: Cursor<Vec<u8>>,
        clock: Arc<AtomicU64>,
        jump_to: u64,
    }

    impl Read for SlowReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.read(buf)?;
            if n == 0 {
                self.clock.store(self.jump_to, Ordering::SeqCst);
            }
            Ok(n)
        }
    }

    #[test]
    fn commit_after_the_reservation_expired_is_refused_and_leaves_nothing() {
        let dir = tmpdir("expiry");
        let clock = Arc::new(AtomicU64::new(NOW));
        let c = Arc::clone(&clock);
        let s = LocalAssetStore::open(&dir, small())
            .unwrap()
            .with_clock(move || c.load(Ordering::SeqCst));
        let mut r = SlowReader {
            data: Cursor::new(png(500, 1)),
            clock: Arc::clone(&clock),
            jump_to: NOW + RESERVATION_TTL_SECS + 1,
        };
        // Request started at NOW; the body finished after the reservation lapsed.
        let out = s.put("s", PNG, None, Some("500"), &mut r, NOW);
        assert_eq!(out, Err(AssetError::ReservationExpired));
        assert_eq!(s.list("s").unwrap(), vec![]);
        assert_eq!(s.usage("s", NOW), Usage::default());
        let tmp_left = fs::read_dir(dir.join("s")).unwrap().count();
        assert_eq!(tmp_left, 0, "temp file removed");
        assert_eq!(fs::read_dir(dir.join(".res")).unwrap().count(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn get_fails_closed_on_a_tampered_file() {
        let dir = tmpdir("tamper");
        let s = store(&dir, CARDLESS_D1);
        let data = png(300, 7);
        let st = put(&s, "s", &data, None).unwrap();
        let path = dir.join("s").join(format!("{}.png", st.sha256));
        let mut bad = data.clone();
        bad[100] ^= 0xFF;
        fs::write(path, bad).unwrap();
        assert_eq!(s.get("s", &st.sha256), Err(AssetError::StoreFailed));
        let _ = fs::remove_dir_all(dir);
    }
}
