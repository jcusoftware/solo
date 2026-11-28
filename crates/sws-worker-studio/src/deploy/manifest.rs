//! Asset manifest for a Direct Upload session (host-testable).
//!
//! Hash format (Direct Upload docs, programmatic example, read 2026-11-27): the first
//! 32 hex characters of `sha256(base64(content) + extension)`, where `extension` is the
//! file extension without the dot (empty when there is none). Manifest keys are
//! absolute paths (`/blog/hello-world.html`) and must list EVERY file the version needs,
//! unchanged ones included; Cloudflare skips re-uploading hashes it already holds.
//!
//! `_headers` and `_redirects` are never served as assets ("This file will not itself be
//! served as a static asset", static-assets headers docs); they travel as
//! `assets.config._headers` / `assets.config._redirects` in the version metadata.
//! `.assetsignore` is a Wrangler-side file and is dropped.

use serde_json::{Map, Value, json};
use sws_assets::hash::sha256_hex;

use super::pipeline::{ArtifactSource, WorkerBundle};

/// Static asset files per Worker version on the Workers Free plan (Paid allows more;
/// Solo stays within the free-tier figure). Limits page, read 2026-11-27.
pub const MAX_FILES: usize = 20_000;
/// Cloudflare's individual static asset file size limit (25 MiB on every plan).
pub const CLOUDFLARE_MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
/// Solo's per-file publish bound, below [`CLOUDFLARE_MAX_FILE_BYTES`]: one file's base64
/// text plus its multipart copy must stay far below the 128 MB isolate limit.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Budget of base64 bytes per upload request; a larger Cloudflare bucket is sent in
/// several requests (each still a subset of that bucket's hashes).
pub const MAX_UPLOAD_REQUEST_B64_BYTES: usize = 12 * 1024 * 1024;
/// Solo's own bound on the raw Worker bundle it holds in memory (not a Cloudflare limit).
pub const MAX_WORKER_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PATH_BYTES: usize = 1024;
/// `_headers` / `_redirects` are small text files; bound what is placed in metadata.
pub const MAX_CONFIG_FILE_BYTES: usize = 64 * 1024;

pub const HEADERS_FILE: &str = "/_headers";
pub const REDIRECTS_FILE: &str = "/_redirects";
pub const ASSETSIGNORE_FILE: &str = "/.assetsignore";

/// Module content types the Version Upload API accepts (API reference, read 2026-11-27).
pub const MODULE_CONTENT_TYPES: &[&str] = &[
    "application/javascript+module",
    "text/javascript+module",
    "application/javascript",
    "text/javascript",
    "application/wasm",
    "text/plain",
    "application/octet-stream",
    "application/source-map",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub code: &'static str,
    pub message: String,
}

fn bad(code: &'static str, message: impl Into<String>) -> ManifestError {
    ManifestError {
        code,
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub content_type: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Sorted by path.
    pub entries: Vec<Entry>,
    pub headers: Option<String>,
    pub redirects: Option<String>,
    worker_lines: Vec<String>,
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648 section 4) with padding.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Extension used in the hash: text after the last dot of the final path segment.
pub fn extension(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or("");
    file.rsplit_once('.').map_or("", |(_, e)| e)
}

/// Cloudflare asset hash of already-base64-encoded content.
pub fn asset_hash_b64(b64: &str, path: &str) -> String {
    let mut pre = String::with_capacity(b64.len() + 8);
    pre.push_str(b64);
    pre.push_str(extension(path));
    sha256_hex(pre.as_bytes())[..32].to_string()
}

pub fn asset_hash(content: &[u8], path: &str) -> String {
    asset_hash_b64(&base64_encode(content), path)
}

/// `Content-Type` attached to an upload part; Cloudflare serves it with the asset.
pub fn content_type(path: &str) -> &'static str {
    match extension(path).to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "jsonld" => "application/ld+json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Absolute, normalized, unambiguous asset path.
pub fn valid_path(path: &str) -> bool {
    path.len() <= MAX_PATH_BYTES
        && path.starts_with('/')
        && path.len() > 1
        && !path.ends_with('/')
        && !path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | '?' | '#'))
        && path[1..]
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn valid_module_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn validate_worker(w: &WorkerBundle) -> Result<(), ManifestError> {
    if w.modules.is_empty() || !w.modules.iter().any(|m| m.name == w.main_module) {
        return Err(bad(
            "artifact_bad_worker",
            "main module missing from the bundle",
        ));
    }
    let mut total = 0usize;
    for (i, m) in w.modules.iter().enumerate() {
        if !valid_module_name(&m.name) || w.modules[..i].iter().any(|o| o.name == m.name) {
            return Err(bad(
                "artifact_bad_worker",
                "invalid or duplicate module name",
            ));
        }
        if !MODULE_CONTENT_TYPES.contains(&m.content_type.as_str()) {
            return Err(bad(
                "artifact_bad_worker",
                "unsupported module content type",
            ));
        }
        total = total.saturating_add(m.bytes.len());
    }
    if total > MAX_WORKER_BYTES {
        return Err(bad("artifact_bad_worker", "worker bundle too large"));
    }
    Ok(())
}

fn config_text(bytes: Vec<u8>, path: &str) -> Result<String, ManifestError> {
    if bytes.len() > MAX_CONFIG_FILE_BYTES {
        return Err(bad("artifact_bad_config", format!("{path} too large")));
    }
    String::from_utf8(bytes).map_err(|_| bad("artifact_bad_config", format!("{path} is not UTF-8")))
}

impl Manifest {
    /// Read every artifact file once (one file in memory at a time), hash it, and
    /// split out the config files.
    pub async fn build<A: ArtifactSource>(a: &A) -> Result<Self, ManifestError> {
        validate_worker(a.worker())?;
        let mut paths = a.paths();
        paths.sort();
        paths.dedup();
        let mut entries = Vec::with_capacity(paths.len());
        let (mut headers, mut redirects) = (None, None);
        for path in paths {
            if !valid_path(&path) {
                return Err(bad("artifact_bad_path", "invalid asset path"));
            }
            let bytes = a
                .read(&path)
                .await
                .map_err(|_| bad("artifact_unreadable", "artifact file could not be read"))?;
            match path.as_str() {
                HEADERS_FILE => headers = Some(config_text(bytes, &path)?),
                REDIRECTS_FILE => redirects = Some(config_text(bytes, &path)?),
                ASSETSIGNORE_FILE => {}
                _ => {
                    let size = bytes.len() as u64;
                    if size > MAX_FILE_BYTES {
                        return Err(bad(
                            "artifact_file_too_large",
                            "an asset exceeds the 10 MiB publish bound",
                        ));
                    }
                    entries.push(Entry {
                        hash: asset_hash(&bytes, &path),
                        content_type: content_type(&path),
                        path,
                        size,
                    });
                    if entries.len() > MAX_FILES {
                        return Err(bad("artifact_too_many_files", "more than 20,000 assets"));
                    }
                }
            }
        }
        if entries.is_empty() {
            return Err(bad("artifact_empty", "no assets to publish"));
        }
        let w = a.worker();
        let mut worker_lines = vec![format!("main\t{}", w.main_module)];
        for m in &w.modules {
            worker_lines.push(format!(
                "module\t{}\t{}\t{}",
                m.name,
                m.content_type,
                sha256_hex(&m.bytes)
            ));
        }
        Ok(Self {
            entries,
            headers,
            redirects,
            worker_lines,
        })
    }

    /// Body of the upload-session request.
    pub fn session_body(&self) -> Value {
        let mut m = Map::new();
        for e in &self.entries {
            m.insert(e.path.clone(), json!({ "hash": e.hash, "size": e.size }));
        }
        json!({ "manifest": Value::Object(m) })
    }

    /// First entry with this hash (identical content at several paths shares a hash).
    pub fn by_hash(&self, hash: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.hash == hash)
    }

    /// Stable digest of everything the version will contain: assets, config files and
    /// the Worker bundle. Recorded on the retained version and checked against the
    /// job's pin.
    pub fn digest(&self) -> String {
        let mut s = String::new();
        for e in &self.entries {
            s.push_str(&format!("asset\t{}\t{}\t{}\n", e.path, e.hash, e.size));
        }
        for (name, body) in [("_headers", &self.headers), ("_redirects", &self.redirects)] {
            if let Some(b) = body {
                s.push_str(&format!("config\t{name}\t{}\n", sha256_hex(b.as_bytes())));
            }
        }
        for l in &self.worker_lines {
            s.push_str(l);
            s.push('\n');
        }
        sha256_hex(s.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(i.as_bytes()), o);
        }
        assert_eq!(base64_encode(&[0xfb, 0xff]), "+/8=");
    }

    /// Reference values computed with Node's crypto exactly as in Cloudflare's example:
    /// `sha256(content.toString("base64") + extension).hex().slice(0, 32)`.
    #[test]
    fn asset_hash_matches_cloudflare_example_algorithm() {
        assert_eq!(
            asset_hash(b"<h1>hi</h1>", "/index.html"),
            "81f20b8a74d6ef9cdfa6039a9db5e33a"
        );
        assert_eq!(
            asset_hash(b"", "/robots.txt"),
            "486b9a6fc855abf48847e9639f3c0908"
        );
        assert_eq!(
            asset_hash(b"body{}", "/a/b.css"),
            "f78a1c20d726372960baaeb9d0aacb0c"
        );
        assert_eq!(
            asset_hash(b"README", "/README"),
            "73f32eb8f77593c0b8591fd0b085690b"
        );
        // The extension is part of the hash: same bytes, different type, different hash.
        assert_ne!(asset_hash(b"x", "/a.css"), asset_hash(b"x", "/a.js"));
    }

    #[test]
    fn extension_uses_the_last_segment_only() {
        assert_eq!(extension("/a.b/c"), "");
        assert_eq!(extension("/x.tar.gz"), "gz");
        assert_eq!(extension("/menu/index.html"), "html");
    }

    #[test]
    fn paths_are_strict() {
        for ok in ["/index.html", "/a/b/c.css", "/_headers", "/img/logo.svg"] {
            assert!(valid_path(ok), "{ok}");
        }
        for bad in [
            "",
            "/",
            "index.html",
            "/a//b",
            "/a/../b",
            "/./a",
            "/a\\b",
            "/a?x",
            "/a#x",
            "/a/",
            "/a\nb",
        ] {
            assert!(!valid_path(bad), "{bad:?}");
        }
        assert!(!valid_path(&format!("/{}", "a".repeat(MAX_PATH_BYTES))));
    }

    #[test]
    fn content_types_cover_site_output() {
        assert_eq!(content_type("/index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("/s.CSS"), "text/css; charset=utf-8");
        assert_eq!(content_type("/logo.svg"), "image/svg+xml");
        assert_eq!(content_type("/blob"), "application/octet-stream");
    }
}
