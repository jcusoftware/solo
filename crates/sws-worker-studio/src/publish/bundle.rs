//! Deterministic, zero-dependency compiled site bundle parsing and byte verification
//! (pure, host-tested).
//!
//! Wire format:
//! - 8-byte magic: `SWSBNDL1`
//! - 4-byte big-endian `u32` header length (`<= BUNDLE_HEADER_CAP`)
//! - UTF-8 JSON header (`BundleHeader`)
//! - Concatenated raw bytes of `worker.modules` in header order, followed by
//!   concatenated raw bytes of `files` in header order.
//!
//! Every static file's actual bytes are hashed with SHA-256 and checked against
//! path rules (rejecting `_headers`, `_redirects`, `_routes.json`, `_worker.js`),
//! size caps, and the expected `manifest_sha256`. Studio's [`BundleArtifactSource`]
//! always injects canonical `/_headers` from `sws_security::headers`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sws_assets::hash::sha256_hex;
use sws_assets::limits::{ArtifactLimits, PUBLISH_ARTIFACT};

use crate::deploy::manifest::{HEADERS_FILE, validate_worker};
use crate::deploy::pipeline::{ArtifactSource, WorkerBundle, WorkerModule};

use super::StoredArtifact;
use super::manifest::{
    Compiler, FileEntry, MANIFEST_FORMAT, Manifest, ManifestError, ValidManifest, revalidate,
    validate,
};

pub const BUNDLE_MAGIC: &[u8; 8] = b"SWSBNDL1";
pub const BUNDLE_HEADER_CAP: usize = 512 * 1024;
pub const MAX_BUNDLE_BYTES: usize = PUBLISH_ARTIFACT.max_total_bytes as usize + BUNDLE_HEADER_CAP;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleHeader {
    pub name: String,
    pub content_type: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerHeader {
    pub main_module: String,
    pub modules: Vec<ModuleHeader>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileHeader {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleHeader {
    pub revision_id: String,
    pub revision_sha256: String,
    pub compiler: Compiler,
    pub schema_level: i64,
    pub worker: WorkerHeader,
    pub files: Vec<FileHeader>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBundle {
    pub valid_manifest: ValidManifest,
    pub worker: WorkerBundle,
    pub schema_level: i64,
    /// Relative path (e.g. `index.html`) -> raw file bytes.
    pub files: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    Malformed,
    Manifest(ManifestError),
    Worker(&'static str),
    InvalidSchemaLevel,
    ManifestMismatch,
}

impl BundleError {
    pub fn status(&self) -> u16 {
        match self {
            BundleError::ManifestMismatch => 422,
            _ => 400,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            BundleError::Malformed => "bundle_malformed",
            BundleError::Manifest(e) => e.code(),
            BundleError::Worker(c) => c,
            BundleError::InvalidSchemaLevel => "invalid_schema_level",
            BundleError::ManifestMismatch => "manifest_mismatch",
        }
    }
}

/// Parse a bundle and verify every file's actual bytes, path rules, Worker bundle,
/// and canonical manifest SHA-256.
pub fn parse_and_verify(
    bytes: &[u8],
    expected_manifest_sha256: Option<&str>,
    limits: &ArtifactLimits,
) -> Result<VerifiedBundle, BundleError> {
    if bytes.len() < 12 || &bytes[..8] != BUNDLE_MAGIC {
        return Err(BundleError::Malformed);
    }
    let header_len = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if header_len == 0 || header_len > BUNDLE_HEADER_CAP || 12 + header_len > bytes.len() {
        return Err(BundleError::Malformed);
    }
    let header: BundleHeader =
        serde_json::from_slice(&bytes[12..12 + header_len]).map_err(|_| BundleError::Malformed)?;
    if header.schema_level < 0 {
        return Err(BundleError::InvalidSchemaLevel);
    }

    let mut offset = 12 + header_len;
    let mut modules = Vec::with_capacity(header.worker.modules.len());
    for m in header.worker.modules {
        let sz = usize::try_from(m.size).map_err(|_| BundleError::Malformed)?;
        let end = offset.checked_add(sz).ok_or(BundleError::Malformed)?;
        if end > bytes.len() {
            return Err(BundleError::Malformed);
        }
        modules.push(WorkerModule {
            name: m.name,
            content_type: m.content_type,
            bytes: bytes[offset..end].to_vec(),
        });
        offset = end;
    }
    let worker = WorkerBundle {
        main_module: header.worker.main_module,
        modules,
    };
    validate_worker(&worker).map_err(|e| BundleError::Worker(e.code))?;

    let mut entries = Vec::with_capacity(header.files.len());
    let mut files = BTreeMap::new();
    let mut total_bytes = 0u64;
    for f in header.files {
        let sz = usize::try_from(f.size).map_err(|_| BundleError::Malformed)?;
        let end = offset.checked_add(sz).ok_or(BundleError::Malformed)?;
        if end > bytes.len() {
            return Err(BundleError::Malformed);
        }
        let slice = &bytes[offset..end];
        offset = end;
        let digest = sha256_hex(slice);
        total_bytes = total_bytes.saturating_add(f.size);
        entries.push(FileEntry {
            path: f.path.clone(),
            sha256: digest,
            size: f.size,
        });
        // Duplicate paths are caught by `validate` below.
        files.entry(f.path).or_insert_with(|| slice.to_vec());
    }
    if offset != bytes.len() {
        return Err(BundleError::Malformed);
    }

    let manifest = Manifest {
        format: MANIFEST_FORMAT.to_string(),
        revision_id: header.revision_id,
        revision_sha256: header.revision_sha256,
        compiler: header.compiler,
        file_count: entries.len() as u64,
        total_bytes,
        files: entries,
    };
    let valid_manifest = validate(manifest, limits).map_err(BundleError::Manifest)?;
    if expected_manifest_sha256.is_some_and(|exp| exp != valid_manifest.digest) {
        return Err(BundleError::ManifestMismatch);
    }

    Ok(VerifiedBundle {
        valid_manifest,
        worker,
        schema_level: header.schema_level,
        files,
    })
}

/// Re-verify a stored bundle's bytes against the job's stored canonical manifest.
pub fn verify_against_stored(
    bytes: &[u8],
    stored: &StoredArtifact,
    limits: &ArtifactLimits,
) -> Result<VerifiedBundle, &'static str> {
    let expected = revalidate(&stored.manifest, &stored.manifest_sha256, limits)
        .map_err(|_| "manifest_invalid")?;
    let parsed = parse_and_verify(bytes, Some(&stored.manifest_sha256), limits).map_err(|e| {
        if matches!(e, BundleError::ManifestMismatch) {
            "hash_mismatch"
        } else {
            e.code()
        }
    })?;
    if parsed.valid_manifest.manifest.files != expected.manifest.files
        || parsed.valid_manifest.digest != expected.digest
    {
        return Err("hash_mismatch");
    }
    Ok(parsed)
}

/// Studio [`ArtifactSource`] backed by a verified bundle, always injecting the
/// canonical `/_headers` file from `sws_security::headers`.
pub struct BundleArtifactSource {
    bundle: VerifiedBundle,
}

impl BundleArtifactSource {
    pub fn new(bundle: VerifiedBundle) -> Self {
        Self { bundle }
    }
}

impl ArtifactSource for BundleArtifactSource {
    fn paths(&self) -> Vec<String> {
        let mut out: Vec<String> = self.bundle.files.keys().map(|p| format!("/{p}")).collect();
        out.push(HEADERS_FILE.to_string());
        out
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        if path == HEADERS_FILE {
            return Ok(sws_security::headers::render_headers_file().into_bytes());
        }
        let rel = path.strip_prefix('/').unwrap_or(path);
        self.bundle
            .files
            .get(rel)
            .cloned()
            .ok_or_else(|| format!("missing file: {path}"))
    }

    fn worker(&self) -> &WorkerBundle {
        &self.bundle.worker
    }

    fn schema_level(&self) -> i64 {
        self.bundle.schema_level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::manifest::{Compiler, FileEntry, MANIFEST_FORMAT, Manifest, validate};
    use sws_assets::limits::PUBLISH_ARTIFACT;

    const H: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn build_raw_bundle(files: &[(&str, &[u8])], schema_level: i64) -> Vec<u8> {
        let worker_bytes = b"export default { fetch() { return new Response('ok'); } };";
        let header = BundleHeader {
            revision_id: "rev-1".into(),
            revision_sha256: H.into(),
            compiler: Compiler {
                name: "sws-cli".into(),
                version: "0.2.0".into(),
            },
            schema_level,
            worker: WorkerHeader {
                main_module: "index.js".into(),
                modules: vec![ModuleHeader {
                    name: "index.js".into(),
                    content_type: "application/javascript+module".into(),
                    size: worker_bytes.len() as u64,
                }],
            },
            files: files
                .iter()
                .map(|(p, b)| FileHeader {
                    path: (*p).to_string(),
                    size: b.len() as u64,
                })
                .collect(),
        };
        let hdr = serde_json::to_vec(&header).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(BUNDLE_MAGIC);
        out.extend_from_slice(&(hdr.len() as u32).to_be_bytes());
        out.extend_from_slice(&hdr);
        out.extend_from_slice(worker_bytes);
        for (_, b) in files {
            out.extend_from_slice(b);
        }
        out
    }

    pub fn build_test_bundle(
        files: &[(&str, &[u8])],
        schema_level: i64,
    ) -> (Vec<u8>, ValidManifest) {
        let entries: Vec<FileEntry> = files
            .iter()
            .map(|(p, b)| FileEntry {
                path: (*p).to_string(),
                sha256: sha256_hex(b),
                size: b.len() as u64,
            })
            .collect();
        let total_bytes: u64 = files.iter().map(|(_, b)| b.len() as u64).sum();
        let m = Manifest {
            format: MANIFEST_FORMAT.into(),
            revision_id: "rev-1".into(),
            revision_sha256: H.into(),
            compiler: Compiler {
                name: "sws-cli".into(),
                version: "0.2.0".into(),
            },
            file_count: entries.len() as u64,
            total_bytes,
            files: entries,
        };
        let valid = validate(m, &PUBLISH_ARTIFACT).unwrap();
        (build_raw_bundle(files, schema_level), valid)
    }

    #[test]
    fn verifies_valid_bundle_and_injects_canonical_headers() {
        let (raw, valid) = build_test_bundle(&[("index.html", b"<h1>Hello</h1>")], 4);
        let parsed = parse_and_verify(&raw, Some(&valid.digest), &PUBLISH_ARTIFACT).unwrap();
        assert_eq!(parsed.valid_manifest.digest, valid.digest);
        assert_eq!(parsed.schema_level, 4);

        let src = BundleArtifactSource::new(parsed);
        assert!(src.paths().contains(&"/_headers".to_string()));
        assert!(src.paths().contains(&"/index.html".to_string()));
    }

    #[test]
    fn rejects_tampered_bytes_and_reserved_paths() {
        let (mut raw, valid) = build_test_bundle(&[("index.html", b"<h1>Hello</h1>")], 4);
        // Flip the last byte of index.html.
        *raw.last_mut().unwrap() ^= 0xff;
        assert_eq!(
            parse_and_verify(&raw, Some(&valid.digest), &PUBLISH_ARTIFACT),
            Err(BundleError::ManifestMismatch)
        );

        for reserved in ["_headers", "_redirects", "_routes.json", "_worker.js/a.js"] {
            let raw = build_raw_bundle(&[(reserved, b"x")], 4);
            let err = parse_and_verify(&raw, None, &PUBLISH_ARTIFACT).unwrap_err();
            assert_eq!(err.code(), "reserved_path", "{reserved}");
        }
    }
}
