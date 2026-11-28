//! Minimal `multipart/form-data` encoder (RFC 7578) for the asset upload and version
//! upload requests. Field names and filenames are Solo-controlled (hex hashes,
//! validated module names, `metadata`); anything that would need quoting is refused.

use sws_assets::hash::sha256_hex;

pub struct FormPart<'a> {
    pub name: &'a str,
    pub filename: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub body: &'a [u8],
}

fn safe(s: &str) -> bool {
    !s.is_empty()
        && !s
            .bytes()
            .any(|b| b < 0x20 || b == b'"' || b == b'\\' || b == 0x7f)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Encode the parts. Returns `(content_type_header, body)`, or `None` if a name is
/// unsafe. The boundary is derived from the content and re-derived until no part
/// contains it, so it can never be confused with data.
pub fn encode(parts: &[FormPart<'_>]) -> Option<(String, Vec<u8>)> {
    for p in parts {
        if !safe(p.name)
            || p.filename.is_some_and(|f| !safe(f))
            || p.content_type.is_some_and(|c| !safe(c))
        {
            return None;
        }
    }
    let seed: usize = parts.iter().map(|p| p.body.len()).sum();
    let mut boundary = String::new();
    for nonce in 0u32.. {
        boundary = format!(
            "solo-{}",
            &sha256_hex(format!("{seed}:{nonce}").as_bytes())[..40]
        );
        let delim = format!("--{boundary}");
        if !parts.iter().any(|p| contains(p.body, delim.as_bytes())) {
            break;
        }
    }
    let mut body = Vec::with_capacity(seed + parts.len() * 160);
    for p in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let mut disp = format!("Content-Disposition: form-data; name=\"{}\"", p.name);
        if let Some(f) = p.filename {
            disp.push_str(&format!("; filename=\"{f}\""));
        }
        body.extend_from_slice(disp.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(ct) = p.content_type {
            body.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(p.body);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Some((format!("multipart/form-data; boundary={boundary}"), body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_unsafe_names() {
        let (ct, body) = encode(&[FormPart {
            name: "metadata",
            filename: None,
            content_type: Some("application/json"),
            body: b"{}",
        }])
        .expect("encodes");
        let b = ct
            .strip_prefix("multipart/form-data; boundary=")
            .expect("boundary");
        let text = String::from_utf8(body).expect("utf8");
        assert_eq!(
            text,
            format!(
                "--{b}\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n{{}}\r\n--{b}--\r\n"
            )
        );
        let bad = FormPart {
            name: "a\"b",
            filename: None,
            content_type: None,
            body: b"",
        };
        assert!(encode(&[bad]).is_none());
        let bad = FormPart {
            name: "a",
            filename: Some("x\r\ny"),
            content_type: None,
            body: b"",
        };
        assert!(encode(&[bad]).is_none());
    }

    #[test]
    fn boundary_never_occurs_in_a_body() {
        // "--solo-" + 40 hex = 47 bytes, so a 47-byte body has seed 47 and its first
        // candidate boundary is derived from "47:0". Put exactly that in the body.
        let first = format!("--solo-{}", &sha256_hex(b"47:0")[..40]);
        assert_eq!(first.len(), 47);
        let body = first.as_bytes();
        let (ct, _) = encode(&[FormPart {
            name: "f",
            filename: None,
            content_type: None,
            body,
        }])
        .expect("encodes");
        let chosen = ct
            .strip_prefix("multipart/form-data; boundary=")
            .expect("b");
        assert_ne!(format!("--{chosen}"), first, "first candidate was skipped");
        assert!(!contains(body, format!("--{chosen}").as_bytes()));
    }
}
