//! CLI artifact manifest: schema, validation and canonical digest (pure, host-tested).
//!
//! The CLI (#68) compiles an immutable revision locally and describes the output as
//! a manifest of relative paths, SHA-256 digests and sizes. Studio validates it
//! here, stores the CANONICAL form (files sorted by path, no whitespace) and its
//! SHA-256 as the artifact identity. Nothing here compiles or claims the output was
//! produced by Studio: the artifact's origin is recorded as `cli`.
//!
//! Request body (`POST /api/sites/{ws}/{site}/publish`):
//!
//! ```json
//! { "request_id": "c-01h…",
//!   "manifest": { "format": "sws-artifact-manifest/1",
//!                 "revision_id": "…", "revision_sha256": "<64 hex>",
//!                 "compiler": { "name": "sws-cli", "version": "0.2.0" },
//!                 "files": [ { "path": "index.html", "sha256": "<64 hex>", "size": 1234 } ],
//!                 "file_count": 1, "total_bytes": 1234 } }
//! ```
//!
//! `request_id` is the client's idempotency key: a retried request with the same key
//! and manifest returns the same job; the same key with another manifest is refused.

use serde::{Deserialize, Serialize};
use sws_assets::hash::sha256_hex;
use sws_assets::limits::ArtifactLimits;
use sws_assets::validate::{is_id, is_sha256};

pub const MANIFEST_FORMAT: &str = "sws-artifact-manifest/1";

/// Cloudflare control files a CLI artifact may not carry. Studio owns them: the
/// security headers come from the site header profile (AGENTS.md section 4) when the
/// artifact is activated (#66), so a published build can never weaken them, and no
/// artifact can ship Worker code or routing rules.
pub const RESERVED_ROOT_FILES: &[&str] = &["_headers", "_redirects", "_routes.json"];
/// A first path segment that is never allowed (file or directory form).
pub const RESERVED_ROOT_SEGMENTS: &[&str] = &["_worker.js"];
/// The only dot-directory a site may publish.
pub const ALLOWED_DOT_SEGMENT: &str = ".well-known";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Compiler {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub revision_id: String,
    pub revision_sha256: String,
    pub compiler: Compiler,
    pub files: Vec<FileEntry>,
    pub file_count: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub request_id: String,
    pub manifest: Manifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// Not JSON, wrong types, unknown or duplicate fields.
    Malformed,
    UnsupportedFormat,
    InvalidRequestId,
    InvalidRevisionId,
    InvalidCompiler,
    Empty,
    TooManyFiles,
    /// `revision_sha256` or a file digest is not 64 lowercase hex chars.
    InvalidSha256 {
        index: Option<usize>,
    },
    InvalidPath {
        index: usize,
    },
    ReservedPath {
        index: usize,
    },
    DuplicatePath {
        index: usize,
    },
    FileTooLarge {
        index: usize,
    },
    ArtifactTooLarge,
    CountMismatch,
    TotalMismatch,
}

impl ManifestError {
    pub fn code(&self) -> &'static str {
        match self {
            ManifestError::Malformed => "manifest_malformed",
            ManifestError::UnsupportedFormat => "manifest_format_unsupported",
            ManifestError::InvalidRequestId => "invalid_request_id",
            ManifestError::InvalidRevisionId => "invalid_revision_id",
            ManifestError::InvalidCompiler => "invalid_compiler",
            ManifestError::Empty => "artifact_empty",
            ManifestError::TooManyFiles => "too_many_files",
            ManifestError::InvalidSha256 { .. } => "invalid_sha256",
            ManifestError::InvalidPath { .. } => "invalid_path",
            ManifestError::ReservedPath { .. } => "reserved_path",
            ManifestError::DuplicatePath { .. } => "duplicate_path",
            ManifestError::FileTooLarge { .. } => "file_too_large",
            ManifestError::ArtifactTooLarge => "artifact_too_large",
            ManifestError::CountMismatch => "file_count_mismatch",
            ManifestError::TotalMismatch => "total_bytes_mismatch",
        }
    }

    /// Index of the offending `files[]` entry, when there is one.
    pub fn index(&self) -> Option<usize> {
        match *self {
            ManifestError::InvalidSha256 { index } => index,
            ManifestError::InvalidPath { index }
            | ManifestError::ReservedPath { index }
            | ManifestError::DuplicatePath { index }
            | ManifestError::FileTooLarge { index } => Some(index),
            _ => None,
        }
    }
}

/// A validated manifest in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidManifest {
    pub manifest: Manifest,
    /// Canonical JSON (files sorted by path); what D1 stores.
    pub canonical: String,
    /// SHA-256 of `canonical`: the artifact identity.
    pub digest: String,
}

fn is_label(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

fn is_segment_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~')
}

/// Path classification for one manifest entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathCheck {
    Ok,
    Invalid,
    Reserved,
}

/// A relative, `/`-separated path of `[A-Za-z0-9._~-]` segments: no leading or
/// trailing `/`, no empty, `.` or `..` segment, no backslash, `%`, NUL, control or
/// non-ASCII byte, no dot-segment other than `.well-known`, within the length and
/// depth caps. Such a path cannot escape the artifact root on any store.
pub fn check_path(path: &str, limits: &ArtifactLimits) -> PathCheck {
    if path.is_empty() || path.len() > limits.max_path_bytes {
        return PathCheck::Invalid;
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > limits.max_path_depth {
        return PathCheck::Invalid;
    }
    for seg in &segments {
        if seg.is_empty() || !seg.bytes().all(is_segment_char) {
            return PathCheck::Invalid;
        }
        if seg.starts_with('.') && *seg != ALLOWED_DOT_SEGMENT {
            // Also covers "." and "..".
            return PathCheck::Invalid;
        }
    }
    let first = segments[0];
    if (segments.len() == 1 && RESERVED_ROOT_FILES.contains(&first))
        || RESERVED_ROOT_SEGMENTS.contains(&first)
    {
        return PathCheck::Reserved;
    }
    PathCheck::Ok
}

/// Parse the request body. JSON errors, unknown fields and duplicate keys are
/// [`ManifestError::Malformed`].
pub fn parse_request(body: &[u8]) -> Result<PublishRequest, ManifestError> {
    serde_json::from_slice(body).map_err(|_| ManifestError::Malformed)
}

/// Validate and canonicalise a manifest.
pub fn validate(m: Manifest, limits: &ArtifactLimits) -> Result<ValidManifest, ManifestError> {
    if m.format != MANIFEST_FORMAT {
        return Err(ManifestError::UnsupportedFormat);
    }
    if !is_id(&m.revision_id) {
        return Err(ManifestError::InvalidRevisionId);
    }
    if !is_sha256(&m.revision_sha256) {
        return Err(ManifestError::InvalidSha256 { index: None });
    }
    if !is_label(&m.compiler.name) || !is_label(&m.compiler.version) {
        return Err(ManifestError::InvalidCompiler);
    }
    if m.files.is_empty() {
        return Err(ManifestError::Empty);
    }
    if m.files.len() > limits.max_files as usize {
        return Err(ManifestError::TooManyFiles);
    }
    let mut total: u64 = 0;
    let mut folded: Vec<(String, usize)> = Vec::with_capacity(m.files.len());
    for (index, f) in m.files.iter().enumerate() {
        match check_path(&f.path, limits) {
            PathCheck::Ok => {}
            PathCheck::Invalid => return Err(ManifestError::InvalidPath { index }),
            PathCheck::Reserved => return Err(ManifestError::ReservedPath { index }),
        }
        if !is_sha256(&f.sha256) {
            return Err(ManifestError::InvalidSha256 { index: Some(index) });
        }
        if f.size > limits.max_file_bytes {
            return Err(ManifestError::FileTooLarge { index });
        }
        total = total.saturating_add(f.size);
        folded.push((f.path.to_ascii_lowercase(), index));
    }
    // Exact duplicates, and paths that collide on a case-insensitive filesystem
    // (the CLI's local preview on macOS/Windows), are both refused.
    folded.sort();
    if let Some(w) = folded.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(ManifestError::DuplicatePath {
            index: w[0].1.max(w[1].1),
        });
    }
    if total > limits.max_total_bytes {
        return Err(ManifestError::ArtifactTooLarge);
    }
    if m.file_count != m.files.len() as u64 {
        return Err(ManifestError::CountMismatch);
    }
    if m.total_bytes != total {
        return Err(ManifestError::TotalMismatch);
    }
    let mut manifest = m;
    manifest.files.sort_by(|a, b| a.path.cmp(&b.path));
    let canonical = serde_json::to_string(&manifest).map_err(|_| ManifestError::Malformed)?;
    if canonical.len() > limits.max_manifest_bytes {
        return Err(ManifestError::ArtifactTooLarge);
    }
    let digest = sha256_hex(canonical.as_bytes());
    Ok(ValidManifest {
        manifest,
        canonical,
        digest,
    })
}

/// Validate a full request: the idempotency key plus the manifest.
pub fn validate_request(
    req: PublishRequest,
    limits: &ArtifactLimits,
) -> Result<(String, ValidManifest), ManifestError> {
    if !is_id(&req.request_id) {
        return Err(ManifestError::InvalidRequestId);
    }
    Ok((req.request_id, validate(req.manifest, limits)?))
}

/// Re-check a STORED canonical manifest (the job's `validating` phase): it must
/// still parse, satisfy today's limits, be canonical, and hash to `stored_digest`.
pub fn revalidate(
    canonical: &str,
    stored_digest: &str,
    limits: &ArtifactLimits,
) -> Result<ValidManifest, ManifestError> {
    let m: Manifest = serde_json::from_str(canonical).map_err(|_| ManifestError::Malformed)?;
    let v = validate(m, limits)?;
    if v.canonical != canonical || v.digest != stored_digest {
        return Err(ManifestError::Malformed);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sws_assets::limits::PUBLISH_ARTIFACT as L;

    const H: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn file(path: &str, size: u64) -> Value {
        json!({ "path": path, "sha256": H, "size": size })
    }

    fn body(files: Vec<Value>) -> Value {
        let total: u64 = files.iter().map(|f| f["size"].as_u64().unwrap()).sum();
        json!({
            "request_id": "req-1",
            "manifest": {
                "format": MANIFEST_FORMAT,
                "revision_id": "rev-1",
                "revision_sha256": H,
                "compiler": { "name": "sws-cli", "version": "0.2.0" },
                "file_count": files.len(),
                "total_bytes": total,
                "files": files,
            }
        })
    }

    fn check(v: &Value) -> Result<(String, ValidManifest), ManifestError> {
        validate_request(parse_request(v.to_string().as_bytes())?, &L)
    }

    fn err(v: &Value) -> ManifestError {
        check(v).expect_err("must be rejected")
    }

    #[test]
    fn a_valid_manifest_is_canonicalised_and_hashed() {
        let v = body(vec![file("styles/site.css", 10), file("index.html", 20)]);
        let (rid, m) = check(&v).unwrap();
        assert_eq!(rid, "req-1");
        let paths: Vec<_> = m.manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["index.html", "styles/site.css"], "sorted by path");
        assert_eq!(m.digest, sha256_hex(m.canonical.as_bytes()));
        assert!(!m.canonical.contains(' ') && !m.canonical.contains('\n'));
        assert!(
            !m.canonical.contains("request_id"),
            "the key is not part of the artifact"
        );
        // Order of the input does not change the identity.
        let (_, again) = check(&body(vec![
            file("index.html", 20),
            file("styles/site.css", 10),
        ]))
        .unwrap();
        assert_eq!(again.digest, m.digest);
        // The stored form re-validates to itself.
        assert_eq!(revalidate(&m.canonical, &m.digest, &L).unwrap(), m);
    }

    #[test]
    fn path_traversal_and_hostile_names_are_refused() {
        for p in [
            "../etc/passwd",
            "a/../../b",
            "..",
            ".",
            "./index.html",
            "a/./b",
            "/index.html",
            "index.html/",
            "a//b",
            "",
            "a\\b",
            "..\\..\\x",
            "%2e%2e/x",
            "a%2fb",
            "a\0b",
            "a\nb",
            "caf\u{e9}.html",
            "a b.html",
            ".git/config",
            ".env",
            "x/.htaccess",
            "C:/x",
        ] {
            assert_eq!(check_path(p, &L), PathCheck::Invalid, "{p:?}");
            assert_eq!(
                err(&body(vec![file("index.html", 1), file(p, 1)])).code(),
                "invalid_path",
                "{p:?}"
            );
        }
        assert_eq!(
            check_path(&"a".repeat(L.max_path_bytes + 1), &L),
            PathCheck::Invalid
        );
        assert_eq!(check_path(&"a".repeat(L.max_path_bytes), &L), PathCheck::Ok);
        let deep = vec!["d"; L.max_path_depth + 1].join("/");
        assert_eq!(check_path(&deep, &L), PathCheck::Invalid);
        for ok in [
            "index.html",
            "a/b/c.min.js",
            ".well-known/security.txt",
            "img/logo~2.png",
            "UPPER/Case.HTML",
            "_next/x.js",
        ] {
            assert_eq!(check_path(ok, &L), PathCheck::Ok, "{ok}");
        }
    }

    #[test]
    fn studio_owned_control_files_are_reserved() {
        for p in [
            "_headers",
            "_redirects",
            "_routes.json",
            "_worker.js",
            "_worker.js/index.js",
        ] {
            assert_eq!(check_path(p, &L), PathCheck::Reserved, "{p}");
            let e = err(&body(vec![file(p, 1)]));
            assert_eq!((e.code(), e.index()), ("reserved_path", Some(0)), "{p}");
        }
        // Only at the root: a nested file of that name is ordinary content.
        assert_eq!(check_path("docs/_headers", &L), PathCheck::Ok);
    }

    #[test]
    fn duplicate_paths_are_refused_including_case_folded_ones() {
        let e = err(&body(vec![
            file("a.html", 1),
            file("b.html", 1),
            file("a.html", 1),
        ]));
        assert_eq!((e.code(), e.index()), ("duplicate_path", Some(2)));
        let e = err(&body(vec![file("Index.html", 1), file("index.html", 1)]));
        assert_eq!(e.code(), "duplicate_path");
    }

    #[test]
    fn bad_hex_is_refused() {
        for bad in [
            "A".repeat(64),
            "g".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
            String::new(),
            format!("{} ", "a".repeat(63)),
        ] {
            let mut v = body(vec![file("index.html", 1)]);
            v["manifest"]["files"][0]["sha256"] = json!(bad);
            let e = err(&v);
            assert_eq!(
                (e.code(), e.index()),
                ("invalid_sha256", Some(0)),
                "{bad:?}"
            );
            let mut v = body(vec![file("index.html", 1)]);
            v["manifest"]["revision_sha256"] = json!(bad);
            assert_eq!(err(&v), ManifestError::InvalidSha256 { index: None });
        }
    }

    #[test]
    fn oversize_files_and_artifacts_are_refused() {
        let e = err(&body(vec![file("big.bin", L.max_file_bytes + 1)]));
        assert_eq!((e.code(), e.index()), ("file_too_large", Some(0)));
        assert!(check(&body(vec![file("max.bin", L.max_file_bytes)])).is_ok());
        let n = (L.max_total_bytes / L.max_file_bytes) as usize + 1;
        let files = (0..n)
            .map(|i| file(&format!("f{i}.bin"), L.max_file_bytes))
            .collect();
        assert_eq!(err(&body(files)).code(), "artifact_too_large");
        // A size that cannot be represented is malformed, not wrapped around.
        let mut v = body(vec![file("a", 1)]);
        v["manifest"]["files"][0]["size"] = json!(-1);
        assert_eq!(err(&v), ManifestError::Malformed);
        let raw = body(vec![file("a", 1)])
            .to_string()
            .replace("\"size\":1", "\"size\":18446744073709551616");
        assert_eq!(
            parse_request(raw.as_bytes()).unwrap_err(),
            ManifestError::Malformed
        );
    }

    #[test]
    fn too_many_files_and_empty_artifacts_are_refused() {
        let files = (0..=L.max_files)
            .map(|i| file(&format!("f{i}"), 0))
            .collect();
        assert_eq!(err(&body(files)).code(), "too_many_files");
        let files: Vec<Value> = (0..L.max_files)
            .map(|i| file(&format!("f{i}"), 0))
            .collect();
        assert!(check(&body(files)).is_ok(), "exactly the cap is fine");
        assert_eq!(err(&body(vec![])).code(), "artifact_empty");
    }

    #[test]
    fn mismatched_totals_are_refused() {
        let mut v = body(vec![file("a", 1), file("b", 2)]);
        v["manifest"]["file_count"] = json!(3);
        assert_eq!(err(&v).code(), "file_count_mismatch");
        let mut v = body(vec![file("a", 1), file("b", 2)]);
        v["manifest"]["total_bytes"] = json!(4);
        assert_eq!(err(&v).code(), "total_bytes_mismatch");
        let mut v = body(vec![file("a", 1), file("b", 2)]);
        v["manifest"]["total_bytes"] = json!(2);
        assert_eq!(err(&v).code(), "total_bytes_mismatch");
    }

    #[test]
    fn schema_is_strict() {
        let good = body(vec![file("a", 1)]);
        // Unknown fields at every level.
        for ptr in ["", "/manifest", "/manifest/compiler", "/manifest/files/0"] {
            let mut v = good.clone();
            v.pointer_mut(ptr).unwrap()["server_verified"] = json!(true);
            assert_eq!(err(&v), ManifestError::Malformed, "extra field at {ptr:?}");
        }
        // Missing fields and wrong types.
        let mut v = good.clone();
        v["manifest"].as_object_mut().unwrap().remove("revision_id");
        assert_eq!(err(&v), ManifestError::Malformed);
        let mut v = good.clone();
        v["manifest"]["files"] = json!({});
        assert_eq!(err(&v), ManifestError::Malformed);
        // Duplicate keys.
        let dup = good.to_string().replacen(
            "\"request_id\":\"req-1\"",
            "\"request_id\":\"req-1\",\"request_id\":\"req-2\"",
            1,
        );
        assert_eq!(
            parse_request(dup.as_bytes()).unwrap_err(),
            ManifestError::Malformed
        );
        assert_eq!(
            parse_request(b"not json").unwrap_err(),
            ManifestError::Malformed
        );
        assert_eq!(parse_request(b"").unwrap_err(), ManifestError::Malformed);
        // Format, ids and compiler labels.
        let mut v = good.clone();
        v["manifest"]["format"] = json!("sws-artifact-manifest/2");
        assert_eq!(err(&v).code(), "manifest_format_unsupported");
        for bad in ["", "Req", "a/b", &"r".repeat(65)] {
            let mut v = good.clone();
            v["request_id"] = json!(bad);
            assert_eq!(err(&v).code(), "invalid_request_id", "{bad:?}");
            let mut v = good.clone();
            v["manifest"]["revision_id"] = json!(bad);
            assert_eq!(err(&v).code(), "invalid_revision_id", "{bad:?}");
        }
        for bad in ["", "sws cli", "<script>", &"v".repeat(65)] {
            let mut v = good.clone();
            v["manifest"]["compiler"]["version"] = json!(bad);
            assert_eq!(err(&v).code(), "invalid_compiler", "{bad:?}");
        }
    }

    #[test]
    fn revalidation_detects_tampering_and_non_canonical_rows() {
        let (_, m) = check(&body(vec![file("b", 2), file("a", 1)])).unwrap();
        assert!(
            revalidate(&m.canonical, &"0".repeat(64), &L).is_err(),
            "digest mismatch"
        );
        let tampered = m.canonical.replace("\"size\":2", "\"size\":3");
        assert!(revalidate(&tampered, &m.digest, &L).is_err());
        // Same content, not canonical (unsorted, or pretty-printed).
        let unsorted = serde_json::to_string(&Manifest {
            files: vec![m.manifest.files[1].clone(), m.manifest.files[0].clone()],
            ..m.manifest.clone()
        })
        .unwrap();
        assert!(revalidate(&unsorted, &sha256_hex(unsorted.as_bytes()), &L).is_err());
        // A limit tightened after enqueue fails the stored artifact.
        let tight = ArtifactLimits {
            max_file_bytes: 1,
            ..L
        };
        assert_eq!(
            revalidate(&m.canonical, &m.digest, &tight)
                .unwrap_err()
                .code(),
            "file_too_large"
        );
    }

    #[test]
    fn a_full_size_canonical_manifest_fits_the_stored_cap() {
        // Worst case the cap allows: max files with max-length paths. Either the
        // canonical form fits `max_manifest_bytes` or validation refuses it; a row
        // can never exceed the D1 CHECK in the migration.
        let long = |i: usize| format!("{i:05}{}", "p".repeat(L.max_path_bytes - 5));
        let files = (0..L.max_files as usize)
            .map(|i| file(&long(i), 0))
            .collect();
        match check(&body(files)) {
            Ok((_, m)) => assert!(m.canonical.len() <= L.max_manifest_bytes),
            Err(e) => assert_eq!(e.code(), "artifact_too_large"),
        }
    }
}
