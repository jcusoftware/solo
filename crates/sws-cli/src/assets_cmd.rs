//! `solo assets`: the offline, native-only local asset store (`sws_assets::local`).
//!
//! Same limits and algorithm as the Studio Worker's cardless D1 store (1 MiB per
//! asset, 50 MiB per site, 150 MiB per store), but on the local filesystem. The
//! Worker never links this code: filesystem access stays out of Wasm.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Subcommand;
use sws_assets::limits;
use sws_assets::local::LocalAssetStore;
use sws_assets::validate::ContentKind;

#[derive(Debug, Subcommand)]
pub enum AssetsAction {
    /// Store a file (content-addressed by SHA-256) and print its address.
    Put {
        /// Image or woff2 file; the type comes from its extension and is checked against its bytes.
        file: PathBuf,
    },
    /// Write a stored asset to a file.
    Get {
        /// Lowercase hex SHA-256.
        sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// List stored assets and usage against the limits.
    Ls,
    /// Delete a stored asset.
    Rm { sha256: String },
    /// Reconcile: drop expired reservations and leftover partial uploads.
    Gc,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn mime_of(file: &Path) -> Result<&'static str, String> {
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .map(|e| if e == "jpeg" { "jpg".to_string() } else { e })
        .unwrap_or_default();
    ContentKind::from_ext(&ext)
        .map(ContentKind::mime)
        .ok_or_else(|| {
            "unsupported_content_type: allowed extensions are png, jpg, gif, webp, avif, woff2"
                .to_string()
        })
}

pub fn run(store: &Path, site: &str, action: AssetsAction) -> Result<(), String> {
    let s = LocalAssetStore::open(store, limits::LOCAL).map_err(|e| e.to_string())?;
    let now = now_secs();
    match action {
        AssetsAction::Put { file } => {
            let mime = mime_of(&file)?;
            let len = fs::metadata(&file)
                .map_err(|e| format!("{}: {e}", file.display()))?
                .len();
            let mut f = fs::File::open(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let stored = s
                .put(site, Some(mime), None, Some(&len.to_string()), &mut f, now)
                .map_err(|e| e.to_string())?;
            println!(
                "{} {} bytes {}{}",
                stored.sha256,
                stored.byte_size,
                stored.kind.mime(),
                if stored.created {
                    ""
                } else {
                    " (already stored)"
                }
            );
        }
        AssetsAction::Get { sha256, out } => {
            let (_, bytes) = s.get(site, &sha256).map_err(|e| e.to_string())?;
            fs::write(&out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
        }
        AssetsAction::Ls => {
            for e in s.list(site).map_err(|e| e.to_string())? {
                println!("{} {} {}", e.sha256, e.byte_size, e.kind.mime());
            }
            let u = s.usage(site, now);
            let l = limits::LOCAL;
            println!(
                "site {} / {} bytes, store {} / {} bytes (provisional limits)",
                u.site_bytes, l.max_site_bytes, u.workspace_bytes, l.max_workspace_bytes
            );
        }
        AssetsAction::Rm { sha256 } => s.delete(site, &sha256).map_err(|e| e.to_string())?,
        AssetsAction::Gc => {
            let r = s.reconcile();
            println!(
                "expired reservations: {}, partial uploads removed: {}, unreadable reservations removed: {}",
                r.expired_reservations, r.tmp_files_removed, r.unreadable_reservations_removed
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn put_get_rm_round_trip_and_limits() {
        let dir = std::env::temp_dir().join(format!("solo-assets-cmd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = dir.join("store");
        let mut data = PNG_SIG.to_vec();
        data.extend_from_slice(&[7; 100]);
        let img = dir.join("a.PNG");
        fs::write(&img, &data).unwrap();
        run(&store, "site-1", AssetsAction::Put { file: img.clone() }).unwrap();
        let sha = sws_assets::hash::sha256_hex(&data);
        let out = dir.join("out.png");
        run(
            &store,
            "site-1",
            AssetsAction::Get {
                sha256: sha.clone(),
                out: out.clone(),
            },
        )
        .unwrap();
        assert_eq!(fs::read(&out).unwrap(), data);
        run(&store, "site-1", AssetsAction::Ls).unwrap();
        run(
            &store,
            "site-1",
            AssetsAction::Rm {
                sha256: sha.clone(),
            },
        )
        .unwrap();
        assert!(
            run(&store, "site-1", AssetsAction::Get { sha256: sha, out })
                .unwrap_err()
                .starts_with("asset_not_found")
        );

        // Over the 1 MiB per-asset cap, a bad extension, and bytes that are not the declared type.
        let big = dir.join("big.png");
        let mut b = PNG_SIG.to_vec();
        b.resize(1024 * 1024 + 1, 1);
        fs::write(&big, b).unwrap();
        assert!(
            run(&store, "site-1", AssetsAction::Put { file: big })
                .unwrap_err()
                .starts_with("asset_too_large")
        );
        let svg = dir.join("x.svg");
        fs::write(&svg, "<svg/>").unwrap();
        assert!(
            run(&store, "site-1", AssetsAction::Put { file: svg })
                .unwrap_err()
                .starts_with("unsupported_content_type")
        );
        let fake = dir.join("fake.png");
        fs::write(&fake, "<html>").unwrap();
        assert!(
            run(&store, "site-1", AssetsAction::Put { file: fake })
                .unwrap_err()
                .starts_with("content_type_mismatch")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
