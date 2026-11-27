//! `solo edit` — explicit, file-based entry points to the canonical editor
//! reducer (`sws_ir::edit`). All file I/O lives here; the reducer is pure.
//!
//! Every subcommand reads its inputs once, computes the result fully in memory
//! and only then writes the explicit `--out` path(s). Inputs are never written
//! unless the caller names the same path as `--out`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use sws_ir::edit::{self, EditOptions};

use crate::commands::{default_registry, parse_value_to_ir_unmerged};

fn read(path: &Path, what: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("failed to read {what} '{}': {e}", path.display()))
}

fn to_json_text<T: Serialize>(path: &Path, value: &T) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("failed to serialize '{}': {e}", path.display()))?;
    text.push('\n');
    Ok(text)
}

/// True when `a` and `b` name the same file (or would after creation): paths
/// are compared after resolving relative segments and symlinks, and existing
/// files also by device and inode (hard links).
fn same_file(a: &Path, b: &Path) -> bool {
    let resolve = |p: &Path| -> Option<PathBuf> {
        if let Ok(canonical) = fs::canonicalize(p) {
            return Some(canonical);
        }
        let name = p.file_name()?;
        let parent = p.parent().filter(|d| !d.as_os_str().is_empty());
        let parent = fs::canonicalize(parent.unwrap_or_else(|| Path::new("."))).ok()?;
        Some(parent.join(name))
    };
    if let (Some(x), Some(y)) = (resolve(a), resolve(b))
        && x == y
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (fs::metadata(a), fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    false
}

/// Refuse a write target that is (or is the same file as) one of `forbidden`,
/// or that already holds a pack manifest: lowered IR is never written over a
/// venue manifest, whatever the path spelling.
fn guard_target(path: &Path, forbidden: &[&Path]) -> Result<(), String> {
    if let Some(input) = forbidden.iter().find(|input| same_file(input, path)) {
        return Err(format!(
            "refusing to write '{}': it is the same file as the input '{}'",
            path.display(),
            input.display()
        ));
    }
    if let Ok(existing) = fs::read_to_string(path)
        && let Ok(raw) = serde_json::from_str::<Value>(&existing)
        && default_registry()
            .list()
            .iter()
            .any(|plugin| plugin.can_transform_input(&raw))
    {
        return Err(format!(
            "refusing to write '{}': it is a pack manifest and edited IR is never written over a manifest",
            path.display()
        ));
    }
    Ok(())
}

/// Resolve a symlinked target to the file it points at so the rename replaces
/// the file, not the link.
fn resolve_link(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

struct Staged {
    temp: PathBuf,
    dest: PathBuf,
}

fn stage(path: &Path, content: &str, index: usize) -> Result<Staged, String> {
    let dest = resolve_link(path);
    let dir = dest
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let name = dest
        .file_name()
        .map_or_else(|| "out".into(), |n| n.to_string_lossy().into_owned());
    let temp = dir.join(format!(".{name}.solo-tmp-{}-{index}", std::process::id()));
    let fail = |e: std::io::Error| format!("failed to write '{}': {e}", path.display());
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(fail)?;
    let written = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| match fs::metadata(&dest) {
            Ok(meta) => fs::set_permissions(&temp, meta.permissions()),
            Err(_) => Ok(()),
        });
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(fail(e));
    }
    Ok(Staged { temp, dest })
}

/// Write every `(path, content)` pair atomically per file: all contents are
/// first written to temp files in the destination directories (so a disk,
/// permission or path problem is found before anything is replaced), then each
/// temp file is renamed over its destination in the order given. Callers list
/// sidecar files (transaction, report) before the main `--out`, so a failure
/// never leaves a new `--out` without its sidecars. The only remaining window
/// is a rename failing part-way, which leaves the earlier files replaced and
/// the later ones untouched (never half-written).
fn write_all(files: &[(&Path, String)]) -> Result<(), String> {
    let mut staged: Vec<Staged> = Vec::new();
    for (index, (path, content)) in files.iter().enumerate() {
        match stage(path, content, index) {
            Ok(item) => staged.push(item),
            Err(e) => {
                for item in &staged {
                    let _ = fs::remove_file(&item.temp);
                }
                return Err(e);
            }
        }
    }
    let mut result = Ok(());
    for item in &staged {
        if result.is_ok()
            && let Err(e) = fs::rename(&item.temp, &item.dest)
        {
            result = Err(format!("failed to write '{}': {e}", item.dest.display()));
        }
        if result.is_err() {
            let _ = fs::remove_file(&item.temp);
        }
    }
    result
}

/// Sorted `metadata` keys (other than the stable-ID marker) of a document.
fn metadata_keys(metadata: &std::collections::BTreeMap<String, Value>) -> Vec<&str> {
    metadata
        .keys()
        .map(String::as_str)
        .filter(|key| *key != edit::MARKER_KEY)
        .collect()
}

/// One-way lowering of a pack manifest (for example `venue.json`) to editable
/// site IR. The result is never written back over the manifest, and it is
/// saved before pack plugins merge their components and forms into it.
pub fn cmd_edit_eject(input: &Path, out: &Path) -> Result<(), String> {
    let raw: Value = serde_json::from_str(&read(input, "input")?)
        .map_err(|e| format!("invalid JSON in '{}': {e}", input.display()))?;
    let (doc, kind) = parse_value_to_ir_unmerged(&raw, &default_registry())?;
    if kind == "site_ir" {
        return Err(format!(
            "'{}' is already site IR; use `solo edit upgrade` to assign stable IDs",
            input.display()
        ));
    }
    guard_target(out, &[input])?;
    write_all(&[(out, edit::canonical_json(&doc))])?;
    let keys = metadata_keys(&doc.metadata);
    let stale = if keys.is_empty() {
        String::new()
    } else {
        format!(
            "; metadata keys copied from the manifest are stale once the IR is edited: {}",
            keys.join(", ")
        )
    };
    println!(
        "solo edit eject: lowered {kind} '{}' to site IR '{}' (one-way{stale})",
        input.display(),
        out.display()
    );
    Ok(())
}

/// Assign stable node IDs to a legacy IR file, writing the result to `out`.
pub fn cmd_edit_upgrade(
    input: &Path,
    out: &Path,
    report: Option<&Path>,
    allow_lossy: bool,
) -> Result<(), String> {
    let source = read(input, "input")?;
    let result =
        edit::upgrade_json(&source, EditOptions { allow_lossy }).map_err(|e| e.to_string())?;
    guard_target(out, &[])?;
    let mut files = Vec::new();
    if let Some(path) = report {
        guard_target(path, &[input, out])?;
        files.push((path, to_json_text(path, &result.report)?));
    }
    files.push((out, result.document.clone()));
    write_all(&files)?;
    println!(
        "solo edit upgrade: assigned {} stable id(s), kept {}, wrote '{}'",
        result.report.assigned.len(),
        result.report.kept,
        out.display()
    );
    if let Ok(doc) = serde_json::from_str::<sws_ir::SiteDocument>(&result.document) {
        let keys = metadata_keys(&doc.metadata);
        if !keys.is_empty() {
            println!(
                "metadata keys preserved unchanged (copies made elsewhere go stale when the IR is edited): {}",
                keys.join(", ")
            );
        }
    }
    if !result.dropped_paths.is_empty() {
        println!(
            "dropped unsupported fields (--allow-lossy): {}",
            result.dropped_paths.join(", ")
        );
    }
    Ok(())
}

/// Apply a command batch file atomically, writing the new document to `out`
/// and, optionally, the transaction (forward, inverse, touched) to `transaction`.
pub fn cmd_edit_apply(
    input: &Path,
    batch: &Path,
    out: &Path,
    transaction: Option<&Path>,
    allow_lossy: bool,
) -> Result<(), String> {
    let source = read(input, "input")?;
    let batch_json = read(batch, "batch")?;
    let result = edit::apply_batch_json(&source, &batch_json, EditOptions { allow_lossy })
        .map_err(|e| e.to_string())?;
    guard_target(out, &[batch])?;
    let mut files = Vec::new();
    if let Some(path) = transaction {
        guard_target(path, &[input, batch, out])?;
        files.push((path, to_json_text(path, &result.transaction)?));
    }
    files.push((out, result.document.clone()));
    write_all(&files)?;
    println!(
        "solo edit apply: {} command(s), {} touched field(s), wrote '{}'",
        result.transaction.forward.len(),
        result.transaction.touched.len(),
        out.display()
    );
    if !result.dropped_paths.is_empty() {
        println!(
            "dropped unsupported fields (--allow-lossy): {}",
            result.dropped_paths.join(", ")
        );
    }
    Ok(())
}
