//! String-in/string-out entry points shared by the CLI and the Wasm bridge.
//!
//! No file or network I/O happens here: callers read the inputs and decide
//! where (and whether) to write the returned document.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::command::{CommandBatch, CommandError, ErrorCode, Transaction};
use super::ids::{UpgradeReport, assign_stable_ids};
use super::lossless::{check_lossless, lossy_paths};
use super::reducer::apply_batch;
use crate::types::SiteDocument;

/// Options for the JSON entry points.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditOptions {
    /// Proceed even when saving would drop unsupported fields; the dropped
    /// paths are then reported in the output instead of failing.
    pub allow_lossy: bool,
}

/// Output of [`apply_batch_json`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditOutput {
    /// Canonical document JSON (pretty, trailing newline).
    pub document: String,
    pub transaction: Transaction,
    /// Fields dropped on save (only non-empty with `allow_lossy`).
    pub dropped_paths: Vec<String>,
}

/// Output of [`upgrade_json`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpgradeOutput {
    pub document: String,
    pub report: UpgradeReport,
    pub dropped_paths: Vec<String>,
}

/// Canonical on-disk form of a document: pretty JSON with a trailing newline.
#[must_use]
pub fn canonical_json(doc: &SiteDocument) -> String {
    let mut text = serde_json::to_string_pretty(doc).unwrap_or_default();
    text.push('\n');
    text
}

fn parse_source(ir_json: &str) -> Result<(Value, SiteDocument), CommandError> {
    let raw: Value = serde_json::from_str(ir_json)
        .map_err(|e| CommandError::new(ErrorCode::InvalidJson, format!("invalid IR JSON: {e}")))?;
    let doc: SiteDocument = serde_json::from_value(raw.clone()).map_err(|e| {
        CommandError::new(
            ErrorCode::InvalidJson,
            format!("invalid SiteDocument JSON: {e}"),
        )
    })?;
    Ok((raw, doc))
}

fn lossless_gate(
    raw: &Value,
    doc: &SiteDocument,
    options: EditOptions,
) -> Result<Vec<String>, CommandError> {
    if options.allow_lossy {
        Ok(lossy_paths(raw, doc))
    } else {
        check_lossless(raw, doc).map(|()| Vec::new())
    }
}

/// Parse the batch JSON (strict: unknown fields are rejected).
pub fn parse_batch(batch_json: &str) -> Result<CommandBatch, CommandError> {
    serde_json::from_str(batch_json).map_err(|e| {
        CommandError::new(
            ErrorCode::InvalidJson,
            format!("invalid command batch: {e}"),
        )
    })
}

/// Parse a document, refuse lossy sources, apply one batch atomically and
/// return the canonical document plus the transaction.
pub fn apply_batch_json(
    ir_json: &str,
    batch_json: &str,
    options: EditOptions,
) -> Result<EditOutput, CommandError> {
    let batch = parse_batch(batch_json)?;
    let (raw, doc) = parse_source(ir_json)?;
    let dropped_paths = lossless_gate(&raw, &doc, options)?;
    let (next, transaction) = apply_batch(&doc, &batch)?;
    Ok(EditOutput {
        document: canonical_json(&next),
        transaction,
        dropped_paths,
    })
}

/// Parse a document, refuse lossy sources and run the explicit stable-ID upgrade.
pub fn upgrade_json(ir_json: &str, options: EditOptions) -> Result<UpgradeOutput, CommandError> {
    let (raw, doc) = parse_source(ir_json)?;
    let dropped_paths = lossless_gate(&raw, &doc, options)?;
    let (next, report) = assign_stable_ids(&doc)?;
    Ok(UpgradeOutput {
        document: canonical_json(&next),
        report,
        dropped_paths,
    })
}
