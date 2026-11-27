//! SHA-256 helpers (pure Rust `sha2`).

use sha2::{Digest, Sha256};

pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

/// Deterministic, tenant-unique row id for an asset: a hash over
/// `workspace \0 site \0 sha256`, so no RNG is needed and two workspaces that
/// upload identical bytes never collide on the global `assets.id` primary key.
/// 48 hex chars, prefixed `a` (generated artifacts use `g`) to fit `[a-z0-9_-]{1,64}`.
pub fn asset_row_id(workspace_id: &str, site_id: &str, sha256: &str) -> String {
    format!(
        "a{}",
        &sha256_hex(format!("{workspace_id}\0{site_id}\0{sha256}").as_bytes())[..47]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn row_ids_are_tenant_unique_and_valid() {
        let h = sha256_hex(b"x");
        let a = asset_row_id("ws-a", "s", &h);
        assert_ne!(a, asset_row_id("ws-b", "s", &h));
        assert_ne!(a, asset_row_id("ws-a", "t", &h));
        assert_eq!(a, asset_row_id("ws-a", "s", &h));
        assert_eq!(a.len(), 48);
        assert!(crate::validate::is_id(&a));
    }
}
