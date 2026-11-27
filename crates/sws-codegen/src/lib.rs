//! Host-only generator for the `@solo-website-system/ir-types` package.
//!
//! Everything here is derived from the Rust types: JSON Schemas come from the
//! `schemars` derives behind `sws-ir`'s `jsonschema` feature, and the `.d.ts`
//! is rendered from those schemas. Nothing in this crate is compiled into the
//! Wasm or Worker builds.
//!
//! # Adding a root type
//!
//! Derive `JsonSchema` on the type (behind the `jsonschema` feature) and add
//! one line to [`core_roots`]. Regeneration then picks up every field.
//!
//! Pack schemas (for example the hospitality venue manifest) are produced by
//! the pack crate and written by the `sws-codegen` binary in `sws-cli`, so
//! this crate never depends on a vertical pack.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;
use sws_ir::edit::{CommandBatch, CommandError, Transaction, UpgradeReport};
use sws_ir::jsonschema::root_schema;
use sws_ir::jsonschema::schemars::JsonSchema;
use sws_ir::{IrPatch, SiteDocument};

pub mod ts;
pub mod validate;

/// Repository-relative directory of the generated package.
pub const PACKAGE_DIR: &str = "packages/@solo-website-system/ir-types";

/// Canonical `$id` of the `SiteDocument` schema; defined once in `sws-ir`.
pub use sws_ir::schema::SITE_DOCUMENT_ID;

/// Canonical `$id` of the `IrPatch` schema.
pub const IR_PATCH_ID: &str = "https://solo-website-system.example/schemas/ir-patch.v1.json";

/// Canonical `$id` of the editor `CommandBatch` schema.
pub const COMMAND_BATCH_ID: &str =
    "https://solo-website-system.example/schemas/command-batch.v1.json";

/// Canonical `$id` of the editor `Transaction` schema.
pub const TRANSACTION_ID: &str = "https://solo-website-system.example/schemas/transaction.v1.json";

/// Canonical `$id` of the editor `UpgradeReport` schema.
pub const UPGRADE_REPORT_ID: &str =
    "https://solo-website-system.example/schemas/upgrade-report.v1.json";

/// Canonical `$id` of the editor `CommandError` schema.
pub const COMMAND_ERROR_ID: &str =
    "https://solo-website-system.example/schemas/command-error.v1.json";

/// One generated file, relative to the repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub path: String,
    pub contents: String,
}

/// A root type of the IR contract: schema file stem plus its schema.
pub struct Root {
    pub file_stem: &'static str,
    pub schema: Value,
}

/// THE REGISTRY. Add a root type of the IR contract by adding one line here.
#[must_use]
pub fn core_roots() -> Vec<Root> {
    vec![
        root::<SiteDocument>("site-document", SITE_DOCUMENT_ID),
        root::<IrPatch>("ir-patch", IR_PATCH_ID),
        root::<CommandBatch>("command-batch", COMMAND_BATCH_ID),
        root::<Transaction>("transaction", TRANSACTION_ID),
        root::<UpgradeReport>("upgrade-report", UPGRADE_REPORT_ID),
        root::<CommandError>("command-error", COMMAND_ERROR_ID),
    ]
}

fn root<T: JsonSchema>(file_stem: &'static str, id: &str) -> Root {
    // IR optionals serialize as `null`, so the schema keeps `T | null`.
    Root {
        file_stem,
        schema: root_schema::<T>(id, true),
    }
}

/// Pretty-print JSON the way every generated file is written (2 spaces, final
/// newline). Object keys are sorted because `serde_json` is built without
/// `preserve_order`, which keeps output deterministic.
#[must_use]
pub fn json_pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// All files of the core package: schemas, declarations and `package.json`.
///
/// `README.md` is hand-written and intentionally not generated.
///
/// # Errors
/// Returns a message if two roots declare the same type name differently.
pub fn core_artifacts() -> Result<Vec<Artifact>, String> {
    let roots = core_roots();
    let mut out = Vec::new();
    for r in &roots {
        out.push(Artifact {
            path: format!("{PACKAGE_DIR}/schema/{}.schema.json", r.file_stem),
            contents: json_pretty(&r.schema),
        });
    }
    let schemas: Vec<Value> = roots.iter().map(|r| r.schema.clone()).collect();
    out.push(Artifact {
        path: format!("{PACKAGE_DIR}/index.d.ts"),
        contents: ts::render_declarations(&schemas)?,
    });
    out.push(Artifact {
        path: format!("{PACKAGE_DIR}/package.json"),
        contents: package_json(&roots),
    });
    // The package declares `MIT OR Apache-2.0`, so it ships both license texts
    // (copied from the repository root at compile time).
    out.push(Artifact {
        path: format!("{PACKAGE_DIR}/LICENSE-MIT"),
        contents: include_str!("../../../LICENSE-MIT").to_string(),
    });
    out.push(Artifact {
        path: format!("{PACKAGE_DIR}/LICENSE-APACHE"),
        contents: include_str!("../../../LICENSE-APACHE").to_string(),
    });
    Ok(out)
}

/// `package.json` is written from a template (not `serde_json`) so the key
/// order stays conventional: name first, then version, license, entry points.
fn package_json(roots: &[Root]) -> String {
    let schema_exports: String = roots
        .iter()
        .map(|r| {
            let rel = format!("./schema/{}.schema.json", r.file_stem);
            format!(",\n    \"{rel}\": \"{rel}\"")
        })
        .collect();
    format!(
        r#"{{
  "name": "@solo-website-system/ir-types",
  "version": "{version}",
  "description": "TypeScript declarations and JSON Schemas (Draft 2020-12) for the Solo Website System IR, generated from the Rust types. Declarations and JSON only; no JavaScript.",
  "license": "MIT OR Apache-2.0",
  "repository": {{
    "type": "git",
    "url": "https://github.com/jcusoftware/solo.git",
    "directory": "{PACKAGE_DIR}"
  }},
  "types": "./index.d.ts",
  "exports": {{
    ".": {{
      "types": "./index.d.ts"
    }}{schema_exports}
  }},
  "files": [
    "index.d.ts",
    "schema/*.schema.json",
    "README.md",
    "LICENSE-MIT",
    "LICENSE-APACHE"
  ],
  "sideEffects": false
}}
"#,
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// How [`sync`] treats the files on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Write every artifact.
    Write,
    /// Leave disk untouched and report drift.
    Check,
}

/// Write or check `artifacts` below `repo_root`. Returns the paths whose
/// on-disk content differs from (or, in `Write` mode, was replaced by) the
/// generated content.
///
/// A `*.schema.json` file that sits next to a generated schema but is not
/// itself generated is stale (a removed or renamed root): it is reported in
/// `Check` mode and deleted in `Write` mode, so it cannot ship via `files`.
///
/// # Errors
/// Returns any I/O error other than a missing file in `Check` mode.
pub fn sync(repo_root: &Path, artifacts: &[Artifact], mode: Mode) -> io::Result<Vec<String>> {
    let mut changed = Vec::new();
    for a in artifacts {
        let path = repo_root.join(&a.path);
        let current = match fs::read_to_string(&path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if current.as_deref() == Some(a.contents.as_str()) {
            continue;
        }
        changed.push(a.path.clone());
        if mode == Mode::Write {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, &a.contents)?;
        }
    }
    let generated: BTreeSet<&str> = artifacts.iter().map(|a| a.path.as_str()).collect();
    let dirs: BTreeSet<&str> = artifacts
        .iter()
        .filter(|a| a.path.ends_with(".schema.json"))
        .filter_map(|a| a.path.rsplit_once('/').map(|(dir, _)| dir))
        .collect();
    for dir in dirs {
        let entries = match fs::read_dir(repo_root.join(dir)) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let mut stale = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = format!("{dir}/{name}");
            if name.ends_with(".schema.json") && !generated.contains(rel.as_str()) {
                stale.push((rel, entry.path()));
            }
        }
        stale.sort();
        for (rel, path) in stale {
            changed.push(rel);
            if mode == Mode::Write {
                fs::remove_file(path)?;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;
