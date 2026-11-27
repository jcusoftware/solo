//! `solo edit` — explicit, file-based entry points to the canonical editor
//! reducer (`sws_ir::edit`). All file I/O lives here; the reducer is pure.
//!
//! Every subcommand reads its inputs once, computes the result fully in memory
//! and only then writes the explicit `--out` path(s). Inputs are never written
//! unless the caller names the same path as `--out`.

use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use sws_ir::edit::{self, EditOptions};

use crate::commands::{default_registry, parse_value_to_ir};

fn read(path: &Path, what: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("failed to read {what} '{}': {e}", path.display()))
}

fn write(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|e| format!("failed to write '{}': {e}", path.display()))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("failed to serialize '{}': {e}", path.display()))?;
    text.push('\n');
    write(path, &text)
}

/// One-way lowering of a pack manifest (for example `venue.json`) to editable
/// site IR. The result is never written back over the manifest.
pub fn cmd_edit_eject(input: &Path, out: &Path) -> Result<(), String> {
    let raw: Value = serde_json::from_str(&read(input, "input")?)
        .map_err(|e| format!("invalid JSON in '{}': {e}", input.display()))?;
    let (doc, kind) = parse_value_to_ir(&raw, &default_registry())?;
    if kind == "site_ir" {
        return Err(format!(
            "'{}' is already site IR; use `solo edit upgrade` to assign stable IDs",
            input.display()
        ));
    }
    write(out, &edit::canonical_json(&doc))?;
    println!(
        "solo edit eject: lowered {kind} '{}' to site IR '{}' (one-way; metadata.venue_manifest is now stale)",
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
    write(out, &result.document)?;
    if let Some(path) = report {
        write_json(path, &result.report)?;
    }
    println!(
        "solo edit upgrade: assigned {} stable id(s), kept {}, wrote '{}'",
        result.report.assigned.len(),
        result.report.kept,
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
    write(out, &result.document)?;
    if let Some(path) = transaction {
        write_json(path, &result.transaction)?;
    }
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
