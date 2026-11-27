//! `sws-codegen`: regenerate (or check) the files derived from the Rust types.
//!
//! * `@solo-website-system/ir-types`: `.d.ts`, JSON Schemas, `package.json`
//! * with `pack-hospitality`: `packs/hospitality/schema/venue.schema.json`
//!
//! Run from the repository root (or pass `--root`):
//!
//! ```text
//! sws-codegen            # write
//! sws-codegen --check    # exit 1 and list files that drifted
//! ```
//!
//! Generation is pure Rust; no Node is involved. Pack schemas are produced by
//! the pack crate and only written here, so `crates/` never depends on a pack
//! outside this feature-gated binary.

use std::path::PathBuf;
use std::process::ExitCode;

use sws_codegen::{Artifact, Mode, core_artifacts, sync, validate};

const USAGE: &str = "usage: sws-codegen [--check] [--root <repo-root>]";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("sws-codegen: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut mode = Mode::Write;
    let mut root = PathBuf::from(".");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => mode = Mode::Check,
            "--root" => {
                root = PathBuf::from(args.next().ok_or_else(|| USAGE.to_string())?);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }

    let mut artifacts = core_artifacts()?;
    artifacts.extend(pack_artifacts());

    for a in &artifacts {
        if a.path.ends_with(".schema.json") {
            ensure_supported(a)?;
        }
    }

    let changed = sync(&root, &artifacts, mode).map_err(|e| e.to_string())?;
    match (mode, changed.is_empty()) {
        (Mode::Check, true) => {
            println!(
                "codegen: {} generated files are up to date",
                artifacts.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        (Mode::Check, false) => {
            eprintln!("codegen drift: these files differ from the Rust types:");
            for path in &changed {
                eprintln!("  {path}");
            }
            eprintln!("run `npm run codegen` and commit the result");
            Ok(ExitCode::FAILURE)
        }
        (Mode::Write, _) => {
            println!(
                "codegen: wrote {} of {} files",
                changed.len(),
                artifacts.len()
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Every generated schema must be self-consistent: only keywords the
/// repository validator understands, and every `$ref` must resolve.
fn ensure_supported(a: &Artifact) -> Result<(), String> {
    let schema: serde_json::Value =
        serde_json::from_str(&a.contents).map_err(|e| format!("{}: {e}", a.path))?;
    let unsupported = validate::unsupported_keywords(&schema);
    if !unsupported.is_empty() {
        return Err(format!(
            "{}: unsupported schema keywords {unsupported:?}",
            a.path
        ));
    }
    let dangling = validate::unresolved_refs(&schema);
    if !dangling.is_empty() {
        return Err(format!("{}: unresolved $ref {dangling:?}", a.path));
    }
    if schema.get("$schema").and_then(|s| s.as_str())
        != Some("https://json-schema.org/draft/2020-12/schema")
    {
        return Err(format!("{}: not a Draft 2020-12 schema", a.path));
    }
    Ok(())
}

#[cfg(feature = "pack-hospitality")]
fn pack_artifacts() -> Vec<Artifact> {
    vec![Artifact {
        path: "packs/hospitality/schema/venue.schema.json".to_string(),
        contents: sws_codegen::json_pretty(&sws_pack_hospitality::venue::schema::venue_schema()),
    }]
}

#[cfg(not(feature = "pack-hospitality"))]
fn pack_artifacts() -> Vec<Artifact> {
    Vec::new()
}
