//! Identifier, content-type and storage-key validation.
//!
//! Assets are content-addressed by lowercase hex SHA-256. Every id that reaches a
//! storage key or filesystem path passes [`is_id`] / [`is_sha256`] first, and those
//! charsets exclude `/`, `.`, `\` and NUL, so a key can never traverse.

use crate::error::AssetError;

/// Opaque app ids (`workspaces.id`, `sites.id`): 1..=64 chars of `[a-z0-9_-]`.
pub fn is_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Lowercase 64-char hex SHA-256.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Allowlisted content kinds. SVG, HTML and PDF are deliberately absent: SVG can
/// carry script, and menus must never be PDF-only (AGENTS.md section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Avif,
    Woff2,
}

impl ContentKind {
    pub const ALL: [ContentKind; 6] = [
        ContentKind::Png,
        ContentKind::Jpeg,
        ContentKind::Gif,
        ContentKind::Webp,
        ContentKind::Avif,
        ContentKind::Woff2,
    ];

    pub fn mime(self) -> &'static str {
        match self {
            ContentKind::Png => "image/png",
            ContentKind::Jpeg => "image/jpeg",
            ContentKind::Gif => "image/gif",
            ContentKind::Webp => "image/webp",
            ContentKind::Avif => "image/avif",
            ContentKind::Woff2 => "font/woff2",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            ContentKind::Png => "png",
            ContentKind::Jpeg => "jpg",
            ContentKind::Gif => "gif",
            ContentKind::Webp => "webp",
            ContentKind::Avif => "avif",
            ContentKind::Woff2 => "woff2",
        }
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.mime() == mime)
    }

    pub fn from_ext(ext: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.ext() == ext)
    }
}

/// Parse a `Content-Type` header: media type only (parameters are ignored),
/// case-insensitive, and it must be on the allowlist.
pub fn declared_kind(header: Option<&str>) -> Result<ContentKind, AssetError> {
    let mime = header
        .and_then(|h| h.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase())
        .unwrap_or_default();
    ContentKind::from_mime(&mime).ok_or(AssetError::UnsupportedContentType)
}

/// Identify a file by its leading magic bytes.
pub fn sniff(bytes: &[u8]) -> Option<ContentKind> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ContentKind::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ContentKind::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ContentKind::Gif)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ContentKind::Webp)
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"avif" | b"avis")
    {
        Some(ContentKind::Avif)
    } else if bytes.starts_with(b"wOF2") {
        Some(ContentKind::Woff2)
    } else {
        None
    }
}

/// The declared type must equal what the bytes actually are.
pub fn check_declared_vs_sniffed(declared: ContentKind, bytes: &[u8]) -> Result<(), AssetError> {
    match sniff(bytes) {
        Some(actual) if actual == declared => Ok(()),
        _ => Err(AssetError::ContentTypeMismatch),
    }
}

/// R2 object key `workspace/site/sha256/reservation`. Every upload writes to its OWN
/// key (the reservation id is unique per request), so concurrent uploads, deletes and
/// reconciliation can never act on each other's object: an object key is referenced by
/// at most one `assets.object_key` row, and only unreferenced keys are ever deleted.
/// Rejects anything that is not a plain id.
pub fn r2_key(
    workspace_id: &str,
    site_id: &str,
    sha256: &str,
    reservation_id: &str,
) -> Option<String> {
    (is_id(workspace_id) && is_id(site_id) && is_sha256(sha256) && is_id(reservation_id))
        .then(|| format!("{workspace_id}/{site_id}/{sha256}/{reservation_id}"))
}

/// Is `name` (a key with its `ws/site/` prefix removed) shaped `sha256/reservation`?
pub fn is_r2_object_name(name: &str) -> bool {
    name.split_once('/')
        .is_some_and(|(sha, rid)| is_sha256(sha) && is_id(rid))
}

/// R2 listing prefix for one site.
pub fn r2_site_prefix(workspace_id: &str, site_id: &str) -> Option<String> {
    (is_id(workspace_id) && is_id(site_id)).then(|| format!("{workspace_id}/{site_id}/"))
}

/// Local store file name `<sha256>.<ext>`.
pub fn local_file_name(sha256: &str, kind: ContentKind) -> Option<String> {
    is_sha256(sha256).then(|| format!("{sha256}.{}", kind.ext()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];

    #[test]
    fn ids_and_hashes() {
        assert!(is_id("ws-a_1"));
        for bad in ["", "A", "a/b", "..", "a.b", "a b", "a\0", &"a".repeat(65)] {
            assert!(!is_id(bad), "{bad:?}");
        }
        assert!(is_sha256(&"a".repeat(64)));
        for bad in [&"A".repeat(64), &"a".repeat(63), &"g".repeat(64), "../etc"] {
            assert!(!is_sha256(bad), "{bad:?}");
        }
    }

    #[test]
    fn keys_cannot_traverse() {
        let h = "a".repeat(64);
        assert_eq!(r2_key("ws", "s", &h, "r1"), Some(format!("ws/s/{h}/r1")));
        for (w, s, x, r) in [
            ("..", "s", h.as_str(), "r1"),
            ("ws", "a/b", &h, "r1"),
            ("ws", "s", "../x", "r1"),
            ("ws", "s", &h, "../r"),
            ("ws", "s", &h, ""),
        ] {
            assert_eq!(r2_key(w, s, x, r), None);
        }
        assert!(is_r2_object_name(&format!("{h}/r1")));
        for bad in [h.as_str(), "x/r1", &format!("{h}/a/b"), &format!("{h}/")] {
            assert!(!is_r2_object_name(bad), "{bad}");
        }
        assert_eq!(local_file_name("../x", ContentKind::Png), None);
    }

    #[test]
    fn declared_type_allowlist() {
        assert_eq!(
            declared_kind(Some("IMAGE/PNG; charset=x")),
            Ok(ContentKind::Png)
        );
        for bad in [
            Some("image/svg+xml"),
            Some("text/html"),
            Some("application/pdf"),
            Some(""),
            None,
        ] {
            assert_eq!(
                declared_kind(bad),
                Err(AssetError::UnsupportedContentType),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn sniff_matches_and_rejects_mismatch() {
        assert_eq!(sniff(PNG), Some(ContentKind::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ContentKind::Jpeg));
        assert_eq!(sniff(b"GIF89a.."), Some(ContentKind::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(ContentKind::Webp));
        assert_eq!(sniff(b"\0\0\0\x1cftypavif"), Some(ContentKind::Avif));
        assert_eq!(sniff(b"wOF2...."), Some(ContentKind::Woff2));
        assert_eq!(sniff(b"<html><script>"), None);
        assert_eq!(check_declared_vs_sniffed(ContentKind::Png, PNG), Ok(()));
        assert_eq!(
            check_declared_vs_sniffed(ContentKind::Jpeg, PNG),
            Err(AssetError::ContentTypeMismatch)
        );
        assert_eq!(
            check_declared_vs_sniffed(ContentKind::Png, b"<svg onload=x>"),
            Err(AssetError::ContentTypeMismatch)
        );
    }

    #[test]
    fn mime_and_ext_round_trip() {
        for k in ContentKind::ALL {
            assert_eq!(ContentKind::from_mime(k.mime()), Some(k));
            assert_eq!(ContentKind::from_ext(k.ext()), Some(k));
        }
    }
}
