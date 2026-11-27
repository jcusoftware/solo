//! Native-only local filesystem asset store for offline/local use.
//!
//! This module is compiled out of `wasm32` entirely (`#[cfg(not(target_arch = "wasm32"))]`
//! in `lib.rs`): the Worker never touches a filesystem. It enforces the same limits
//! and the same reserve, stream-count, verify, commit algorithm as the D1 and R2
//! backends (see [`crate::quota`]).
//!
//! Layout under `root` (one store per workspace): `<site>/<sha256>.<ext>` for
//! committed assets and `<site>/.up-<n>.tmp` for in-flight uploads. Committing is an
//! atomic rename performed while holding the reservation lock, so concurrent
//! threads can never over-commit.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::AssetError;
use crate::hash::sha256_hex;
use crate::limits::{AssetLimits, RESERVATION_TTL_SECS};
use crate::quota::{StreamCounter, Usage, diagnose, fits, plan_upload};
use crate::validate::{
    ContentKind, check_declared_vs_sniffed, declared_kind, is_id, is_sha256, local_file_name,
};

#[derive(Debug)]
struct Reservation {
    id: u64,
    site: String,
    bytes: u64,
    expires_at: u64,
}

#[derive(Debug, Default)]
struct State {
    next_id: u64,
    reservations: Vec<Reservation>,
}

#[derive(Debug)]
pub struct LocalAssetStore {
    root: PathBuf,
    limits: AssetLimits,
    state: Mutex<State>,
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
        fs::create_dir_all(&root).map_err(io)?;
        Ok(Self {
            root,
            limits,
            state: Mutex::new(State::default()),
        })
    }

    fn site_dir(&self, site: &str) -> Result<PathBuf, AssetError> {
        if is_id(site) {
            Ok(self.root.join(site))
        } else {
            Err(AssetError::SiteNotFound)
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
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

    fn usage_locked(&self, st: &State, site: &str, now: u64) -> Usage {
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
        for r in st.reservations.iter().filter(|r| r.expires_at > now) {
            u.workspace_bytes += r.bytes;
            if r.site == site {
                u.site_bytes += r.bytes;
                u.site_assets += 1;
            }
        }
        u
    }

    pub fn usage(&self, site: &str, now: u64) -> Usage {
        let st = self.lock();
        self.usage_locked(&st, site, now)
    }

    fn reserve(&self, site: &str, bytes: u64, now: u64) -> Result<u64, AssetError> {
        let mut st = self.lock();
        let usage = self.usage_locked(&st, site, now);
        if !fits(&usage, bytes, &self.limits) {
            return Err(diagnose(&usage, bytes, &self.limits));
        }
        st.next_id += 1;
        let id = st.next_id;
        st.reservations.push(Reservation {
            id,
            site: site.to_string(),
            bytes,
            expires_at: now + RESERVATION_TTL_SECS,
        });
        Ok(id)
    }

    fn release(&self, id: u64) {
        self.lock().reservations.retain(|r| r.id != id);
    }

    /// Store `reader`'s bytes under `site`, content-addressed. `expected_sha256`
    /// (when given) must match the received bytes. `declared_len` is the optional
    /// `Content-Length`-style hint; the bytes actually read are what is enforced.
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
        let result = self.receive(&dir, kind, expected_sha256, plan, reader, rid);
        self.release(rid);
        result
    }

    fn receive(
        &self,
        dir: &Path,
        kind: ContentKind,
        expected: Option<&str>,
        plan: crate::quota::UploadPlan,
        reader: &mut dyn Read,
        rid: u64,
    ) -> Result<Stored, AssetError> {
        fs::create_dir_all(dir).map_err(io)?;
        let tmp = dir.join(format!(".up-{rid}.tmp"));
        let outcome = (|| {
            let mut counter = StreamCounter::new(plan);
            let mut data = Vec::new();
            let mut file = fs::File::create(&tmp).map_err(io)?;
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = reader.read(&mut buf).map_err(io)?;
                if n == 0 {
                    break;
                }
                counter.push(n)?; // reject over-cap mid-stream, before writing more
                file.write_all(&buf[..n]).map_err(io)?;
                data.extend_from_slice(&buf[..n]);
            }
            file.flush().map_err(io)?;
            let size = counter.finish()?;
            check_declared_vs_sniffed(kind, &data)?;
            let sha = sha256_hex(&data);
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
        if dest.exists() {
            let _ = fs::remove_file(&tmp);
            return Ok(Stored {
                sha256: sha,
                byte_size: size,
                kind,
                created: false,
            });
        }
        // Commit while holding the lock so the rename and the reservation release
        // are one step with respect to other reservers.
        let mut st = self.lock();
        let renamed = fs::rename(&tmp, &dest);
        st.reservations.retain(|r| r.id != rid);
        drop(st);
        renamed.map_err(|e| {
            let _ = fs::remove_file(&tmp);
            io(e)
        })?;
        Ok(Stored {
            sha256: sha,
            byte_size: size,
            kind,
            created: true,
        })
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

    pub fn get(&self, site: &str, sha256: &str) -> Result<(ContentKind, Vec<u8>), AssetError> {
        let (p, kind) = self.find(site, sha256)?;
        Ok((kind, fs::read(p).map_err(io)?))
    }

    pub fn delete(&self, site: &str, sha256: &str) -> Result<(), AssetError> {
        let _guard = self.lock();
        let (p, _) = self.find(site, sha256)?;
        fs::remove_file(p).map_err(io)
    }

    pub fn list(&self, site: &str) -> Result<Vec<Entry>, AssetError> {
        let mut v = Self::committed(&self.site_dir(site)?);
        v.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        Ok(v)
    }

    /// Drop expired reservations and tmp files of uploads that are no longer in
    /// flight (crashed or abandoned).
    pub fn reconcile(&self, now: u64) -> ReconcileReport {
        let mut st = self.lock();
        let before = st.reservations.len();
        st.reservations.retain(|r| r.expires_at > now);
        let mut report = ReconcileReport {
            expired_reservations: before - st.reservations.len(),
            ..Default::default()
        };
        let live: Vec<String> = st
            .reservations
            .iter()
            .map(|r| format!(".up-{}.tmp", r.id))
            .collect();
        if let Ok(rd) = fs::read_dir(&self.root) {
            for d in rd.flatten() {
                let Ok(files) = fs::read_dir(d.path()) else {
                    continue;
                };
                for f in files.flatten() {
                    let name = f.file_name().to_string_lossy().into_owned();
                    if name.starts_with(".up-")
                        && name.ends_with(".tmp")
                        && !live.contains(&name)
                        && fs::remove_file(f.path()).is_ok()
                    {
                        report.tmp_files_removed += 1;
                    }
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
        }
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
        let s = LocalAssetStore::open(&dir, CARDLESS_D1).unwrap();
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
        let s = LocalAssetStore::open(&dir, small()).unwrap();
        // No Content-Length: the stream itself is cut at the cap.
        assert_eq!(
            put(&s, "s", &png(2 * 1024 + 1, 1), None),
            Err(AssetError::AssetTooLarge)
        );
        // Lying (too small) Content-Length.
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
        let s = LocalAssetStore::open(&dir, small()).unwrap();
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
        let s = Arc::new(LocalAssetStore::open(&dir, small()).unwrap());
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
        let s = LocalAssetStore::open(&dir, small()).unwrap();
        let rid = s.reserve("a", 2 * KIB, NOW).unwrap();
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a").join(format!(".up-{rid}.tmp")), b"partial").unwrap();
        fs::write(dir.join("a").join(".up-999.tmp"), b"orphan").unwrap();
        // Live: the in-flight tmp file is kept, the orphan is removed.
        assert_eq!(
            s.reconcile(NOW),
            ReconcileReport {
                expired_reservations: 0,
                tmp_files_removed: 1
            }
        );
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
        let _ = fs::remove_dir_all(dir);
    }
}
