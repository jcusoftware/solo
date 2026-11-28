//! The preview bundle format, `sws-preview-bundle/1` (pure, host-tested).
//!
//! A preview is a whole compiled site stored as ONE generated artifact (#63,
//! `GeneratedKind::Preview`), so it shares the existing Preview budget and
//! retention. The bundle is a deterministic, dependency-free container:
//!
//! ```text
//! sws-preview-bundle/1\n
//! <sha256> <size> <path>\n        one line per file, paths strictly ascending (bytewise)
//! ...
//! \n                              an empty line ends the header
//! <bytes of file 1><bytes of file 2>...   concatenated in header order, nothing after
//! ```
//!
//! `sha256` is 64 lowercase hex, `size` is decimal without leading zeros. The
//! header is ASCII and at most [`MAX_HEADER_BYTES`], so it always sits inside
//! the first stored chunk and serving one file needs one header read. The same
//! file set always produces the same bytes ([`build`]), so the artifact digest
//! identifies the content.
//!
//! [`validate`] runs on upload and rejects: a wrong magic line, an oversized or
//! malformed header, more than [`MAX_FILES`] files, a hostile path (same rules
//! as a publish manifest: `[A-Za-z0-9._~-]` segments, no empty, `.` or `..`
//! segment, no dot-segment except `.well-known`, no `%`, backslash, NUL,
//! control or non-ASCII byte), a reserved Cloudflare control file, a file type
//! outside the allowlist ([`FileKind`]), unsorted or duplicate paths, paths
//! that differ only by ASCII case, a file that is also a directory, a size
//! mismatch, a per-file SHA-256 mismatch, binary bytes that do not match their
//! extension, a missing root `index.html`, or a bundle over the Preview cap.

use std::collections::BTreeSet;

use sws_assets::hash::sha256_hex;
use sws_assets::limits::{CHUNK_BYTES, PREVIEW};
use sws_assets::validate::{ContentKind, is_sha256, sniff};

/// First line of every bundle.
pub const MAGIC: &str = "sws-preview-bundle/1";
/// Required `Content-Type` of an upload.
pub const CONTENT_TYPE: &str = "application/vnd.sws.preview-bundle";
/// Largest bundle: the Preview per-artifact cap (#63), never a second budget.
pub const MAX_BUNDLE_BYTES: u64 = PREVIEW.max_artifact_bytes;
/// Files per bundle.
pub const MAX_FILES: usize = 512;
/// Bytes per path.
pub const MAX_PATH_BYTES: usize = 200;
/// Segments per path.
pub const MAX_PATH_DEPTH: usize = 16;
/// Header bytes (magic line through the terminating empty line). Below one
/// stored chunk, so the header is always read from chunk 0.
pub const MAX_HEADER_BYTES: usize = 192 * 1024;
const _: () = assert!(MAX_HEADER_BYTES < CHUNK_BYTES);
/// The only dot-directory a bundle may carry.
pub const ALLOWED_DOT_SEGMENT: &str = ".well-known";
/// Cloudflare control files: a preview never applies them, and a publish
/// artifact may not carry them either, so a bundle may not.
pub const RESERVED_ROOT_FILES: &[&str] = &["_headers", "_redirects", "_routes.json"];
/// A first path segment that is never allowed (file or directory).
pub const RESERVED_ROOT_SEGMENTS: &[&str] = &["_worker.js"];
/// The file served for `/` and for every path ending in `/`.
pub const INDEX_FILE: &str = "index.html";

/// Allowed file types, decided by extension (ASCII case-insensitive). Text
/// types are served with `charset=utf-8`. SVG and XML are allowed: they can
/// carry script, but so can the draft HTML itself, and everything runs on the
/// isolated preview origin under the `preview` CSP (`connect-src 'none'`,
/// `form-action 'none'`, framing by the Studio origin only). PDF, WASM, source
/// maps, archives and extensionless files are refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Html,
    Css,
    Js,
    Json,
    Txt,
    Xml,
    Svg,
    Png,
    Jpeg,
    Gif,
    Webp,
    Avif,
    Ico,
    Woff2,
}

impl FileKind {
    pub fn from_path(path: &str) -> Option<Self> {
        let name = path.rsplit('/').next()?;
        let (stem, ext) = name.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        Some(match ext.to_ascii_lowercase().as_str() {
            "html" => Self::Html,
            "css" => Self::Css,
            "js" => Self::Js,
            "json" => Self::Json,
            "txt" => Self::Txt,
            "xml" => Self::Xml,
            "svg" => Self::Svg,
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "gif" => Self::Gif,
            "webp" => Self::Webp,
            "avif" => Self::Avif,
            "ico" => Self::Ico,
            "woff2" => Self::Woff2,
            _ => return None,
        })
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::Css => "text/css; charset=utf-8",
            Self::Js => "text/javascript; charset=utf-8",
            Self::Json => "application/json",
            Self::Txt => "text/plain; charset=utf-8",
            Self::Xml => "application/xml",
            Self::Svg => "image/svg+xml",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Ico => "image/x-icon",
            Self::Woff2 => "font/woff2",
        }
    }

    /// Binary kinds whose leading bytes must match the extension.
    fn magic_matches(self, bytes: &[u8]) -> bool {
        let want = match self {
            Self::Png => ContentKind::Png,
            Self::Jpeg => ContentKind::Jpeg,
            Self::Gif => ContentKind::Gif,
            Self::Webp => ContentKind::Webp,
            Self::Avif => ContentKind::Avif,
            Self::Woff2 => ContentKind::Woff2,
            Self::Ico => return bytes.starts_with(&[0, 0, 1, 0]),
            _ => return true,
        };
        sniff(bytes) == Some(want)
    }
}

/// One file of a parsed header. `offset` is absolute within the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub offset: u64,
    pub size: u64,
    pub kind: FileKind,
}

/// A parsed, validated header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// Bytes of the header, including the terminating empty line.
    pub header_len: usize,
    /// Sorted by path (bytewise), unique.
    pub files: Vec<FileEntry>,
    /// `header_len` plus every file size: the exact bundle length.
    pub total_len: u64,
}

impl Index {
    pub fn get(&self, path: &str) -> Option<&FileEntry> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.files[i])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleError {
    /// Larger than [`MAX_BUNDLE_BYTES`].
    TooLarge,
    /// Not `sws-preview-bundle/1`.
    BadMagic,
    /// No terminating empty line within [`MAX_HEADER_BYTES`].
    HeaderTooLarge,
    /// A header line is not `<sha256> <size> <path>` (or not ASCII).
    MalformedHeader {
        line: usize,
    },
    Empty,
    TooManyFiles,
    InvalidPath {
        index: usize,
    },
    ReservedPath {
        index: usize,
    },
    UnsupportedType {
        index: usize,
    },
    /// Not strictly ascending: an exact duplicate or out of order.
    UnsortedOrDuplicate {
        index: usize,
    },
    /// Equal to an earlier path ignoring ASCII case.
    CaseDuplicate {
        index: usize,
    },
    /// The path is also a directory of another path (or the reverse).
    PathConflict {
        index: usize,
    },
    InvalidSha256 {
        index: usize,
    },
    InvalidSize {
        index: usize,
    },
    /// Header sizes do not add up to the bundle length.
    LengthMismatch,
    Sha256Mismatch {
        index: usize,
    },
    /// Binary bytes do not match the extension.
    TypeMismatch {
        index: usize,
    },
    MissingIndex,
}

impl BundleError {
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "bundle_too_large",
            Self::BadMagic => "bundle_format_unsupported",
            Self::HeaderTooLarge => "bundle_header_too_large",
            Self::MalformedHeader { .. } => "bundle_malformed",
            Self::Empty => "bundle_empty",
            Self::TooManyFiles => "too_many_files",
            Self::InvalidPath { .. } => "invalid_path",
            Self::ReservedPath { .. } => "reserved_path",
            Self::UnsupportedType { .. } => "unsupported_file_type",
            Self::UnsortedOrDuplicate { .. } => "unsorted_or_duplicate_path",
            Self::CaseDuplicate { .. } => "case_duplicate_path",
            Self::PathConflict { .. } => "path_conflict",
            Self::InvalidSha256 { .. } => "invalid_sha256",
            Self::InvalidSize { .. } => "invalid_size",
            Self::LengthMismatch => "bundle_length_mismatch",
            Self::Sha256Mismatch { .. } => "sha256_mismatch",
            Self::TypeMismatch { .. } => "content_type_mismatch",
            Self::MissingIndex => "missing_index_html",
        }
    }

    pub fn status(self) -> u16 {
        match self {
            Self::TooLarge => 413,
            _ => 400,
        }
    }

    /// Zero-based index of the offending file entry, when there is one. The
    /// path itself is never echoed.
    pub fn index(self) -> Option<usize> {
        match self {
            Self::InvalidPath { index }
            | Self::ReservedPath { index }
            | Self::UnsupportedType { index }
            | Self::UnsortedOrDuplicate { index }
            | Self::CaseDuplicate { index }
            | Self::PathConflict { index }
            | Self::InvalidSha256 { index }
            | Self::InvalidSize { index }
            | Self::Sha256Mismatch { index }
            | Self::TypeMismatch { index } => Some(index),
            _ => None,
        }
    }
}

/// Path classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathCheck {
    Ok,
    Invalid,
    Reserved,
}

fn is_segment_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~')
}

/// A relative `/`-separated path that cannot escape the bundle root on any
/// store or URL resolver (see the module docs for the exact rules).
pub fn check_path(path: &str) -> PathCheck {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return PathCheck::Invalid;
    }
    let mut depth = 0;
    for seg in path.split('/') {
        depth += 1;
        if depth > MAX_PATH_DEPTH || seg.is_empty() || !seg.bytes().all(is_segment_char) {
            return PathCheck::Invalid;
        }
        // Also covers "." and "..".
        if seg.starts_with('.') && seg != ALLOWED_DOT_SEGMENT {
            return PathCheck::Invalid;
        }
    }
    let first = path.split('/').next().unwrap_or("");
    if (depth == 1 && RESERVED_ROOT_FILES.contains(&first))
        || RESERVED_ROOT_SEGMENTS.contains(&first)
    {
        return PathCheck::Reserved;
    }
    PathCheck::Ok
}

/// Decimal without sign, leading zeros (except `0`) or overflow.
fn parse_size(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 10 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    s.parse().ok()
}

/// Parse and check the header at the start of `head` (which need not hold the
/// file bytes). Checks everything that does not need file contents.
pub fn parse_header(head: &[u8]) -> Result<Index, BundleError> {
    let window = &head[..head.len().min(MAX_HEADER_BYTES)];
    let magic = format!("{MAGIC}\n");
    if !window.starts_with(magic.as_bytes()) {
        return Err(
            if window.len() < magic.len() && magic.as_bytes().starts_with(window) {
                BundleError::HeaderTooLarge
            } else {
                BundleError::BadMagic
            },
        );
    }
    // The header ends at the first empty line ("\n\n").
    let Some(end) = window[magic.len() - 1..]
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|p| magic.len() - 1 + p + 2)
    else {
        return Err(BundleError::HeaderTooLarge);
    };
    let body = &window[magic.len()..end - 1];
    let mut files: Vec<FileEntry> = Vec::new();
    let mut folded: BTreeSet<String> = BTreeSet::new();
    let mut offset = end as u64;
    if !body.is_empty() {
        let text = std::str::from_utf8(body)
            .ok()
            .filter(|t| t.is_ascii())
            .ok_or(BundleError::MalformedHeader { line: 1 })?;
        for (i, line) in text
            .strip_suffix('\n')
            .unwrap_or(text)
            .split('\n')
            .enumerate()
        {
            if i >= MAX_FILES {
                return Err(BundleError::TooManyFiles);
            }
            let mut parts = line.splitn(3, ' ');
            let (Some(sha), Some(size), Some(path)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(BundleError::MalformedHeader { line: i + 1 });
            };
            if !is_sha256(sha) {
                return Err(BundleError::InvalidSha256 { index: i });
            }
            let size = parse_size(size).ok_or(BundleError::InvalidSize { index: i })?;
            match check_path(path) {
                PathCheck::Ok => {}
                PathCheck::Invalid => return Err(BundleError::InvalidPath { index: i }),
                PathCheck::Reserved => return Err(BundleError::ReservedPath { index: i }),
            }
            let kind =
                FileKind::from_path(path).ok_or(BundleError::UnsupportedType { index: i })?;
            if files.last().is_some_and(|prev| prev.path.as_str() >= path) {
                return Err(BundleError::UnsortedOrDuplicate { index: i });
            }
            let lower = path.to_ascii_lowercase();
            if !folded.insert(lower.clone()) {
                return Err(BundleError::CaseDuplicate { index: i });
            }
            files.push(FileEntry {
                path: path.to_string(),
                sha256: sha.to_string(),
                offset,
                size,
                kind,
            });
            offset = offset
                .checked_add(size)
                .ok_or(BundleError::InvalidSize { index: i })?;
        }
    }
    if files.is_empty() {
        return Err(BundleError::Empty);
    }
    // A path may not also be a directory of another path ("a" and "a/b").
    for (i, f) in files.iter().enumerate() {
        let lower = f.path.to_ascii_lowercase();
        let mut prefix = String::new();
        for seg in lower.split('/') {
            if !prefix.is_empty() && folded.contains(&prefix) {
                return Err(BundleError::PathConflict { index: i });
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
        }
    }
    Ok(Index {
        header_len: end,
        files,
        total_len: offset,
    })
}

/// Full upload validation: header, exact length, the Preview cap, per-file
/// SHA-256, magic bytes for binary types, and a root `index.html`.
pub fn validate(bundle: &[u8]) -> Result<Index, BundleError> {
    if bundle.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(BundleError::TooLarge);
    }
    let index = parse_header(bundle)?;
    if index.total_len != bundle.len() as u64 {
        return Err(BundleError::LengthMismatch);
    }
    for (i, f) in index.files.iter().enumerate() {
        let start = f.offset as usize;
        let bytes = &bundle[start..start + f.size as usize];
        if sha256_hex(bytes) != f.sha256 {
            return Err(BundleError::Sha256Mismatch { index: i });
        }
        if !f.kind.magic_matches(bytes) {
            return Err(BundleError::TypeMismatch { index: i });
        }
    }
    if index.get(INDEX_FILE).is_none() {
        return Err(BundleError::MissingIndex);
    }
    Ok(index)
}

/// Build the canonical bundle for `files` (sorted by path here). For producers
/// and tests; the result still has to pass [`validate`].
pub fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut sorted: Vec<&(&str, &[u8])> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = format!("{MAGIC}\n").into_bytes();
    for (path, bytes) in &sorted {
        out.extend_from_slice(format!("{} {} {path}\n", sha256_hex(bytes), bytes.len()).as_bytes());
    }
    out.push(b'\n');
    for (_, bytes) in &sorted {
        out.extend_from_slice(bytes);
    }
    out
}

/// Where a request path inside a preview leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved<'a> {
    File(&'a FileEntry),
    /// `rest` names a directory with an `index.html`: redirect to `rest/`.
    AddSlash,
    NotFound,
}

/// Resolve the part of the URL path after `/p/{site}/{revision}/{token}/`.
/// Empty or ending in `/` means that directory's `index.html`. The raw
/// (still percent-encoded) path is checked with [`check_path`], so `%`,
/// backslashes, dot-segments and other hostile forms never match a file.
pub fn resolve<'a>(index: &'a Index, rest: &str) -> Resolved<'a> {
    let candidate = if rest.is_empty() || rest.ends_with('/') {
        format!("{rest}{INDEX_FILE}")
    } else {
        rest.to_string()
    };
    if check_path(&candidate) != PathCheck::Ok {
        return Resolved::NotFound;
    }
    if let Some(f) = index.get(&candidate) {
        return Resolved::File(f);
    }
    if !rest.is_empty()
        && !rest.ends_with('/')
        && index.get(&format!("{rest}/{INDEX_FILE}")).is_some()
    {
        return Resolved::AddSlash;
    }
    Resolved::NotFound
}

/// The stored chunks a byte range touches: `(seq, start, end)` with `start..end`
/// relative to that chunk. Empty for an empty range.
pub fn chunk_span(offset: u64, size: u64, chunk_bytes: usize) -> Vec<(u32, usize, usize)> {
    let cb = chunk_bytes as u64;
    let mut out = Vec::new();
    if size == 0 || cb == 0 {
        return out;
    }
    let end = offset.saturating_add(size);
    let mut pos = offset;
    while pos < end {
        let seq = pos / cb;
        let chunk_start = seq * cb;
        let chunk_end = (chunk_start + cb).min(end);
        out.push((
            seq as u32,
            (pos - chunk_start) as usize,
            (chunk_end - chunk_start) as usize,
        ));
        pos = chunk_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

    fn site() -> Vec<u8> {
        build(&[
            ("index.html", b"<!doctype html><title>Draft</title>"),
            ("styles.css", b"body{color:#241207}"),
            ("events/index.html", b"<!doctype html><title>Events</title>"),
            ("img/logo.png", PNG),
            ("empty.txt", b""),
        ])
    }

    /// A bundle with a hand-written header (sizes and digests taken from `files`
    /// unless the line is given verbatim).
    fn raw(lines: &[String], body: &[u8]) -> Vec<u8> {
        let mut out = format!("{MAGIC}\n").into_bytes();
        for l in lines {
            out.extend_from_slice(l.as_bytes());
            out.push(b'\n');
        }
        out.push(b'\n');
        out.extend_from_slice(body);
        out
    }

    fn line(path: &str, bytes: &[u8]) -> String {
        format!("{} {} {path}", sha256_hex(bytes), bytes.len())
    }

    fn with_index(extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut files: Vec<(&str, &[u8])> = vec![("index.html", b"<p>x</p>")];
        files.extend_from_slice(extra);
        build(&files)
    }

    #[test]
    fn a_canonical_bundle_round_trips() {
        let b = site();
        let idx = validate(&b).expect("valid");
        assert_eq!(idx.total_len, b.len() as u64);
        let paths: Vec<&str> = idx.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "empty.txt",
                "events/index.html",
                "img/logo.png",
                "index.html",
                "styles.css"
            ]
        );
        let css = idx.get("styles.css").unwrap();
        assert_eq!(
            &b[css.offset as usize..(css.offset + css.size) as usize],
            b"body{color:#241207}"
        );
        assert_eq!(css.kind.content_type(), "text/css; charset=utf-8");
        // Deterministic: input order does not matter.
        let again = build(&[
            ("styles.css", b"body{color:#241207}".as_slice()),
            ("img/logo.png", PNG),
            ("empty.txt", b""),
            ("events/index.html", b"<!doctype html><title>Events</title>"),
            ("index.html", b"<!doctype html><title>Draft</title>"),
        ]);
        assert_eq!(again, b);
    }

    #[test]
    fn the_header_alone_parses_without_file_bytes() {
        let b = site();
        let idx = validate(&b).unwrap();
        let head = parse_header(&b[..idx.header_len]).unwrap();
        assert_eq!(head, idx);
    }

    #[test]
    fn hostile_paths_are_refused() {
        let x: &[u8] = b"x";
        for bad in [
            "../index.html",
            "a/../index.html",
            "./a.txt",
            "a/./b.txt",
            "/abs.txt",
            "a//b.txt",
            "a/",
            "a\\b.txt",
            "a%2fb.txt",
            "%2e%2e/a.txt",
            "a\0.txt",
            "caf\u{e9}.txt",
            ".env.txt",
            ".git/config.txt",
            "a/.hidden.txt",
            "a b.txt",
            "a\tb.txt",
            "a\rb.txt",
            "a?b.txt",
            "a#b.txt",
            "a:b.txt",
        ] {
            let b = raw(
                &[line("index.html", b"<p>x</p>"), line(bad, x)],
                b"<p>x</p>x",
            );
            let r = validate(&b);
            assert!(
                matches!(
                    r,
                    Err(BundleError::InvalidPath { .. }
                        | BundleError::MalformedHeader { .. }
                        | BundleError::UnsortedOrDuplicate { .. })
                ),
                "{bad:?} gave {r:?}"
            );
        }
        let long = format!("{}.txt", "a".repeat(MAX_PATH_BYTES));
        assert_eq!(check_path(&long), PathCheck::Invalid);
        let deep = format!("{}a.txt", "d/".repeat(MAX_PATH_DEPTH));
        assert_eq!(check_path(&deep), PathCheck::Invalid);
        assert_eq!(check_path(".well-known/security.txt"), PathCheck::Ok);
    }

    #[test]
    fn reserved_cloudflare_files_and_unknown_types_are_refused() {
        for (p, want) in [
            ("_headers", "reserved_path"),
            ("_redirects", "reserved_path"),
            ("_routes.json", "reserved_path"),
            ("_worker.js", "reserved_path"),
            ("_worker.js/index.js", "reserved_path"),
            ("menu.pdf", "unsupported_file_type"),
            ("app.wasm", "unsupported_file_type"),
            ("app.js.map", "unsupported_file_type"),
            ("README", "unsupported_file_type"),
            (
                ".well-known/apple-app-site-association",
                "unsupported_file_type",
            ),
            ("noext.", "unsupported_file_type"),
        ] {
            let b = with_index(&[(p, b"x")]);
            let e = validate(&b).expect_err(p);
            assert_eq!(e.code(), want, "{p}");
            assert_eq!(e.status(), 400);
        }
        // Not reserved below the root.
        assert_eq!(check_path("docs/_headers.txt"), PathCheck::Ok);
    }

    #[test]
    fn duplicates_case_folds_order_and_conflicts_are_refused() {
        let a: &[u8] = b"a";
        let idx: &[u8] = b"<p>x</p>";
        // Exact duplicate.
        let b = raw(
            &[line("a.txt", a), line("a.txt", a), line("index.html", idx)],
            b"aa<p>x</p>",
        );
        assert_eq!(
            validate(&b),
            Err(BundleError::UnsortedOrDuplicate { index: 1 })
        );
        // Out of order.
        let b = raw(&[line("index.html", idx), line("a.txt", a)], b"<p>x</p>a");
        assert_eq!(
            validate(&b),
            Err(BundleError::UnsortedOrDuplicate { index: 1 })
        );
        // Case-folded duplicate ("A.txt" < "a.txt" bytewise).
        let b = raw(
            &[line("A.txt", a), line("a.txt", a), line("index.html", idx)],
            b"aa<p>x</p>",
        );
        assert_eq!(validate(&b), Err(BundleError::CaseDuplicate { index: 1 }));
        let b = with_index(&[("Index.HTML", b"<p>y</p>")]);
        assert_eq!(validate(&b).unwrap_err().code(), "case_duplicate_path");
        // A file that is also a directory, either way round and across case.
        for (f, d) in [("a.txt", "a.txt/b.txt"), ("docs.html", "DOCS.HTML/x.txt")] {
            let b = with_index(&[(f, b"1"), (d, b"2")]);
            assert_eq!(validate(&b).unwrap_err().code(), "path_conflict", "{f} {d}");
        }
    }

    #[test]
    fn sizes_digests_and_lengths_must_agree() {
        let idx: &[u8] = b"<p>x</p>";
        // Wrong digest.
        // ("z.txt" sorts after "index.html", so its bytes are last.)
        let mut b = with_index(&[("z.txt", b"hello")]);
        let last = b.len() - 1;
        b[last] ^= 1;
        assert_eq!(validate(&b), Err(BundleError::Sha256Mismatch { index: 1 }));
        // Trailing bytes and truncation.
        let mut b = with_index(&[("a.txt", b"hello")]);
        b.push(b'!');
        assert_eq!(validate(&b), Err(BundleError::LengthMismatch));
        let b = with_index(&[("a.txt", b"hello")]);
        assert_eq!(
            validate(&b[..b.len() - 1]),
            Err(BundleError::LengthMismatch)
        );
        // Malformed sizes and digests.
        for size in ["01", "-1", "+1", "1.0", "", "99999999999", "0x10", " 1"] {
            let l = format!("{} {size} index.html", sha256_hex(idx));
            let r = validate(&raw(&[l], idx));
            assert!(
                matches!(
                    r,
                    Err(BundleError::InvalidSize { .. } | BundleError::InvalidPath { .. })
                ),
                "{size:?} gave {r:?}"
            );
        }
        let upper = sha256_hex(idx).to_ascii_uppercase();
        let b = raw(&[format!("{upper} 8 index.html")], idx);
        assert_eq!(validate(&b), Err(BundleError::InvalidSha256 { index: 0 }));
        // Huge declared sizes cannot overflow the offset maths.
        let b = raw(
            &[
                format!("{} 9999999999 a.txt", sha256_hex(b"")),
                format!("{} 9999999999 index.html", sha256_hex(b"")),
            ],
            b"",
        );
        assert_eq!(validate(&b), Err(BundleError::LengthMismatch));
    }

    #[test]
    fn binary_bytes_must_match_their_extension() {
        for (p, bytes) in [
            ("a.png", b"<svg onload=alert(1)>".as_slice()),
            ("a.jpg", PNG),
            ("a.woff2", b"<html>"),
            ("a.ico", b"<html>"),
            ("a.png", b""),
        ] {
            let b = with_index(&[(p, bytes)]);
            assert_eq!(
                validate(&b).unwrap_err().code(),
                "content_type_mismatch",
                "{p}"
            );
        }
        let b = with_index(&[
            ("a.ico", &[0, 0, 1, 0, 1, 0]),
            ("b.JPEG", &[0xFF, 0xD8, 0xFF, 0]),
        ]);
        assert!(validate(&b).is_ok());
    }

    #[test]
    fn magic_header_and_framing_are_strict() {
        assert_eq!(validate(b""), Err(BundleError::HeaderTooLarge));
        assert_eq!(validate(b"sws-preview"), Err(BundleError::HeaderTooLarge));
        assert_eq!(validate(b"PK\x03\x04zip"), Err(BundleError::BadMagic));
        assert_eq!(
            validate(b"sws-preview-bundle/2\n\n"),
            Err(BundleError::BadMagic)
        );
        assert_eq!(
            validate(b"sws-preview-bundle/1\r\n\r\n"),
            Err(BundleError::BadMagic)
        );
        assert_eq!(
            validate(b"sws-preview-bundle/1\n\n"),
            Err(BundleError::Empty)
        );
        // No terminating empty line.
        let mut b = format!("{MAGIC}\n").into_bytes();
        b.extend_from_slice(line("index.html", b"x").as_bytes());
        b.push(b'\n');
        assert_eq!(validate(&b), Err(BundleError::HeaderTooLarge));
        // A header over the cap.
        let mut b = format!("{MAGIC}\n").into_bytes();
        b.resize(MAX_HEADER_BYTES + 10, b'a');
        assert_eq!(validate(&b), Err(BundleError::HeaderTooLarge));
        // CRLF lines, extra spaces and non-ASCII bytes are malformed.
        let idx: &[u8] = b"<p>x</p>";
        let crlf = format!("{}\r", line("index.html", idx));
        assert!(validate(&raw(&[crlf], idx)).is_err());
        let two = format!("{}  {} index.html", sha256_hex(idx), idx.len());
        assert!(validate(&raw(&[two], idx)).is_err());
        let short = sha256_hex(idx).to_string();
        assert_eq!(
            validate(&raw(&[short], idx)),
            Err(BundleError::MalformedHeader { line: 1 })
        );
        let mut b = raw(&[line("index.html", idx)], idx);
        b[MAGIC.len() + 3] = 0xC3;
        assert!(validate(&b).is_err());
    }

    #[test]
    fn file_count_and_total_size_are_capped() {
        let names: Vec<String> = (0..MAX_FILES).map(|i| format!("f{i:04}.txt")).collect();
        let mut files: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), b"".as_slice())).collect();
        files.push(("index.html", b"<p>x</p>"));
        assert_eq!(validate(&build(&files)), Err(BundleError::TooManyFiles));
        files.remove(0);
        assert!(
            validate(&build(&files)).is_ok(),
            "exactly MAX_FILES is allowed"
        );

        let big = vec![b'a'; MAX_BUNDLE_BYTES as usize];
        let b = with_index(&[("big.txt", &big)]);
        assert_eq!(validate(&b), Err(BundleError::TooLarge));
        assert_eq!(BundleError::TooLarge.status(), 413);
    }

    #[test]
    fn a_root_index_is_required() {
        let b = build(&[("events/index.html", b"<p>x</p>")]);
        assert_eq!(validate(&b), Err(BundleError::MissingIndex));
    }

    #[test]
    fn errors_name_the_entry_but_never_echo_the_path() {
        let b = with_index(&[("../x.txt", b"x")]);
        let e = validate(&b).unwrap_err();
        assert_eq!(e.index(), Some(0));
        assert!(!e.code().contains(".."));
    }

    #[test]
    fn request_paths_resolve_to_files_index_or_a_slash_redirect() {
        let b = site();
        let idx = validate(&b).unwrap();
        let file = |rest: &str| match resolve(&idx, rest) {
            Resolved::File(f) => Some(f.path.clone()),
            _ => None,
        };
        assert_eq!(file("").as_deref(), Some("index.html"));
        assert_eq!(file("index.html").as_deref(), Some("index.html"));
        assert_eq!(file("events/").as_deref(), Some("events/index.html"));
        assert_eq!(file("img/logo.png").as_deref(), Some("img/logo.png"));
        assert_eq!(resolve(&idx, "events"), Resolved::AddSlash);
        for missing in [
            "img/",
            "img",
            "nope.html",
            "STYLES.CSS",
            "../index.html",
            "%2e%2e/index.html",
            "events/../index.html",
            "events%2findex.html",
            "./index.html",
            "//index.html",
            "img//logo.png",
            "index.html/",
            "\\index.html",
            "_headers",
        ] {
            assert_eq!(resolve(&idx, missing), Resolved::NotFound, "{missing:?}");
        }
    }

    #[test]
    fn chunk_spans_cover_exactly_the_range() {
        assert!(chunk_span(10, 0, 4).is_empty());
        assert_eq!(chunk_span(0, 4, 4), vec![(0, 0, 4)]);
        assert_eq!(chunk_span(3, 2, 4), vec![(0, 3, 4), (1, 0, 1)]);
        assert_eq!(chunk_span(4, 9, 4), vec![(1, 0, 4), (2, 0, 4), (3, 0, 1)]);
        // Property: concatenated slices equal the range, for every offset/size.
        let data: Vec<u8> = (0..50u8).collect();
        let chunks: Vec<&[u8]> = data.chunks(7).collect();
        for off in 0..50u64 {
            for size in 0..(50 - off) {
                let mut got = Vec::new();
                for (seq, s, e) in chunk_span(off, size, 7) {
                    got.extend_from_slice(&chunks[seq as usize][s..e]);
                }
                assert_eq!(got, &data[off as usize..(off + size) as usize]);
            }
        }
    }
}
