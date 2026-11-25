//! Core command implementations (`init`, `build`, `check`, `export`) for `solo`.

use std::collections::BTreeMap;
#[cfg(feature = "pack-hospitality")]
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sws_compiler::{
    CompileOptions, CompiledSite, DiagnosticLevel, check_site_diagnostics, compile_from_json,
    compile_site_with_raw,
};
use sws_ir::SiteDocument;
#[cfg(feature = "pack-hospitality")]
use sws_pack_hospitality::abc::{CITATIONS as ABC_CITATIONS, lint_json_value};
#[cfg(feature = "pack-hospitality")]
use sws_pack_hospitality::compiler::HospitalityPlugin;
use sws_plugin::PluginRegistry;
use sws_security::headers::{SECURITY_HEADERS, parse_headers_block};

#[cfg(feature = "pack-hospitality")]
use crate::templates::starter_hospitality_venue;
use crate::templates::{
    EXPORT_DEV_VARS_EXAMPLE, MIGRATION_0001_INIT_SQL, render_export_cargo_toml,
    render_export_readme, render_export_worker_lib, render_export_wrangler_jsonc, starter_readme,
    starter_site_document,
};

/// Construct the default [`PluginRegistry`] with compiled-in vertical packs registered.
#[must_use]
pub fn default_registry() -> PluginRegistry {
    #[cfg_attr(not(feature = "pack-hospitality"), allow(unused_mut))]
    let mut registry = PluginRegistry::new();
    #[cfg(feature = "pack-hospitality")]
    registry
        .register(Box::new(HospitalityPlugin))
        .expect("built-in hospitality pack must satisfy the current plugin ABI");
    registry
}

/// Default compilation options used by CLI commands.
#[must_use]
pub fn default_compile_options() -> CompileOptions {
    CompileOptions {
        include_headers_file: true,
        ..CompileOptions::default()
    }
}

/// Merge pack-contributed extra files into compile options (feature wiring).
///
/// This is the allowed `crates/sws-cli` exception (AGENTS.md §1): hospitality
/// venues contribute deterministic pack assets — the events calendar assets
/// (`events-config.json` marker + `js/events-client.js`) when
/// `scheduling.events.enabled`, and the sandbox demo checkout catalog
/// (`demo-checkout-catalog.json`) when `checkout` is configured; every other
/// input compiles unchanged with an empty extra-file map.
pub fn compile_options_with_pack_extras(input_json: &str) -> Result<CompileOptions, String> {
    let mut options = default_compile_options();
    let raw: Value = serde_json::from_str(input_json)
        .map_err(|e| format!("invalid JSON input: {e}"))?;
    options.extra_files = pack_extra_files(&raw)?;
    Ok(options)
}

#[cfg(feature = "pack-hospitality")]
fn pack_extra_files(input: &Value) -> Result<BTreeMap<String, String>, String> {
    // Only hospitality manifests (venue shape) carry pack asset sections.
    if !(input.get("menu").is_some() && input.get("nap").is_some() && input.get("hours").is_some())
    {
        return Ok(BTreeMap::new());
    }
    let venue: sws_pack_hospitality::venue::VenueManifest =
        serde_json::from_value(input.clone())
            .map_err(|e| format!("invalid venue manifest: {e}"))?;
    let mut files =
        sws_pack_hospitality::events::events_extra_files(&venue).unwrap_or_default();
    files.extend(sws_pack_hospitality::bookings::bookings_extra_files(&venue).unwrap_or_default());
    files.extend(sws_pack_hospitality::compiler::checkout_extra_files(&venue));
    Ok(files)
}

#[cfg(not(feature = "pack-hospitality"))]
fn pack_extra_files(_input: &Value) -> Result<BTreeMap<String, String>, String> {
    Ok(BTreeMap::new())
}

/// Structured issue emitted by `solo check` and MCP `run_a11y_and_plugin_checks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckIssue {
    pub source: String,
    pub level: String,
    pub code: String,
    pub path: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation: Option<String>,
}

/// Summary counts for [`CheckReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSummary {
    pub error_count: usize,
    pub warning_count: usize,
    pub plugin_finding_count: usize,
}

/// Structured report returned by `solo check --json` and MCP `run_a11y_and_plugin_checks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub ok: bool,
    pub input: String,
    pub kind: String,
    pub errors: Vec<CheckIssue>,
    pub warnings: Vec<CheckIssue>,
    pub plugin_findings: Vec<CheckIssue>,
    pub summary: CheckSummary,
}

/// Initialize a starter project directory (`solo init`).
pub fn cmd_init(dir: &Path, pack: Option<&str>) -> Result<(), String> {
    let (manifest_name, manifest_json) = match pack {
        #[cfg(feature = "pack-hospitality")]
        Some("hospitality") => {
            let venue = starter_hospitality_venue();
            let json = serde_json::to_string_pretty(&venue)
                .map_err(|e| format!("failed to serialize starter venue manifest: {e}"))?;
            ("venue.json", format!("{json}\n"))
        }
        #[cfg(not(feature = "pack-hospitality"))]
        Some("hospitality") => {
            return Err(
                "pack 'hospitality' is not compiled into this solo binary; rebuild with `--features pack-hospitality`"
                    .to_string(),
            );
        }
        None => {
            let doc = starter_site_document();
            let json = serde_json::to_string_pretty(&doc)
                .map_err(|e| format!("failed to serialize starter site IR: {e}"))?;
            ("site.ir.json", format!("{json}\n"))
        }
        Some(other) => {
            return Err(format!(
                "unsupported pack '{other}'; supported packs: hospitality"
            ));
        }
    };

    fs::create_dir_all(dir)
        .map_err(|e| format!("failed to create directory '{}': {e}", dir.display()))?;

    let manifest_path = dir.join(manifest_name);
    fs::write(&manifest_path, manifest_json)
        .map_err(|e| format!("failed to write '{}': {e}", manifest_path.display()))?;

    let readme_path = dir.join("README.md");
    fs::write(&readme_path, starter_readme(manifest_name))
        .map_err(|e| format!("failed to write '{}': {e}", readme_path.display()))?;

    println!(
        "solo init: created {} and {}",
        manifest_path.display(),
        readme_path.display()
    );
    Ok(())
}

/// Compile a site manifest/IR JSON into `<outdir>` or verify existing files with `--check`.
pub fn cmd_build(input: &Path, outdir: &Path, check: bool) -> Result<(), String> {
    let input_json = fs::read_to_string(input)
        .map_err(|e| format!("failed to read input '{}': {e}", input.display()))?;

    let registry = default_registry();
    let options = compile_options_with_pack_extras(&input_json)?;
    let compiled = compile_from_json(&input_json, &registry, &options)
        .map_err(|e| format!("compilation failed for '{}': {e}", input.display()))?;

    report_compiler_diagnostics(&compiled)?;

    let total_bytes = compiled.index_html_bytes + compiled.styles_css_bytes;
    let suffix = if check { " (check)" } else { "" };
    println!(
        "solo build: index.html {} B + styles.css {} B = {} B{suffix}",
        compiled.index_html_bytes, compiled.styles_css_bytes, total_bytes
    );

    if check {
        let diffs = check_compiled_files(outdir, &compiled.files)?;
        if !diffs.is_empty() {
            for msg in &diffs {
                eprintln!("stale: {msg}");
            }
            return Err(format!(
                "{} file(s) in '{}' are missing or out of date",
                diffs.len(),
                outdir.display()
            ));
        }
    } else {
        write_compiled_files(outdir, &compiled.files)?;
    }

    Ok(())
}

/// Run validation, WCAG/compiler checks, plugin/ABC lint, and optional `_headers` parity check.
pub fn cmd_check(
    input: &Path,
    headers_file: Option<&Path>,
    json_output: bool,
) -> Result<bool, String> {
    let raw_str = fs::read_to_string(input)
        .map_err(|e| format!("failed to read input '{}': {e}", input.display()))?;
    let raw: Value = serde_json::from_str(&raw_str)
        .map_err(|e| format!("invalid JSON in '{}': {e}", input.display()))?;

    let registry = default_registry();
    let report = run_checks_on_value(&raw, &input.display().to_string(), headers_file, &registry);

    if json_output {
        let out = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("failed to serialize check report: {e}"))?;
        println!("{out}");
    } else {
        print_human_check_report(&report);
    }

    Ok(report.ok)
}

/// Export a deployable Cloudflare Workers + Static Assets + D1 bundle (`solo export`).
pub fn cmd_export(input: &Path, outdir: &Path) -> Result<(), String> {
    let input_json = fs::read_to_string(input)
        .map_err(|e| format!("failed to read input '{}': {e}", input.display()))?;
    let raw: Value = serde_json::from_str(&input_json)
        .map_err(|e| format!("invalid JSON in '{}': {e}", input.display()))?;

    let registry = default_registry();
    let (doc, _kind) = parse_value_to_ir(&raw, &registry)?;
    let mut options = default_compile_options();
    options.extra_files = pack_extra_files(&raw)?;
    let compiled = compile_site_with_raw(&doc, Some(&raw), Some(&registry), &options)
        .map_err(|e| format!("compilation failed for '{}': {e}", input.display()))?;

    report_compiler_diagnostics(&compiled)?;

    let public_dir = outdir.join("public");
    write_compiled_files(&public_dir, &compiled.files)?;

    let src_dir = outdir.join("src");
    fs::create_dir_all(&src_dir).map_err(|e| {
        format!(
            "failed to create src directory '{}': {e}",
            src_dir.display()
        )
    })?;
    let worker_lib_path = src_dir.join("lib.rs");
    fs::write(&worker_lib_path, render_export_worker_lib())
        .map_err(|e| format!("failed to write '{}': {e}", worker_lib_path.display()))?;

    let cargo_toml_path = outdir.join("Cargo.toml");
    fs::write(&cargo_toml_path, render_export_cargo_toml(&doc.slug))
        .map_err(|e| format!("failed to write '{}': {e}", cargo_toml_path.display()))?;

    let migrations_dir = outdir.join("migrations");
    fs::create_dir_all(&migrations_dir).map_err(|e| {
        format!(
            "failed to create migrations directory '{}': {e}",
            migrations_dir.display()
        )
    })?;
    let migration_path = migrations_dir.join("0001_init.sql");
    fs::write(&migration_path, MIGRATION_0001_INIT_SQL)
        .map_err(|e| format!("failed to write '{}': {e}", migration_path.display()))?;

    let wrangler_path = outdir.join("wrangler.jsonc");
    let wrangler_content = render_export_wrangler_jsonc(&doc.slug, &doc.base_url);
    fs::write(&wrangler_path, wrangler_content)
        .map_err(|e| format!("failed to write '{}': {e}", wrangler_path.display()))?;

    let dev_vars_path = outdir.join(".dev.vars.example");
    fs::write(&dev_vars_path, EXPORT_DEV_VARS_EXAMPLE)
        .map_err(|e| format!("failed to write '{}': {e}", dev_vars_path.display()))?;

    let readme_path = outdir.join("README.md");
    fs::write(&readme_path, render_export_readme(&doc.slug))
        .map_err(|e| format!("failed to write '{}': {e}", readme_path.display()))?;

    println!(
        "solo export: wrote {} public file(s), standalone Rust Worker ({}, {}), {}, and {}",
        compiled.files.len(),
        cargo_toml_path.display(),
        worker_lib_path.display(),
        wrangler_path.display(),
        migration_path.display()
    );
    Ok(())
}

/// Serve a compiled site directory over local HTTP on 127.0.0.1 (`solo preview`).
///
/// Blocks in the foreground until Ctrl-C (SIGINT) terminates the process.
/// The Cloudflare `_headers` file, if present, is served as a static file and
/// never applied as HTTP headers (it is a Cloudflare-only directive).
pub fn cmd_preview(outdir: &Path, port: u16) -> Result<(), String> {
    crate::preview::serve_forever(outdir, port)
}

/// Convert a parsed JSON value (`SiteDocument` or plugin-supported manifest) into [`SiteDocument`].
pub fn parse_value_to_ir(
    raw: &Value,
    registry: &PluginRegistry,
) -> Result<(SiteDocument, String), String> {
    if registry.list().iter().any(|p| p.can_transform_input(raw)) {
        let mut doc = registry
            .transform_input_with(None, raw)
            .map_err(|e| format!("plugin input transformation failed: {e}"))?;
        registry
            .apply_to_document(&mut doc)
            .map_err(|e| format!("plugin IR transform failed: {e}"))?;
        return Ok((doc, "venue_manifest".to_string()));
    }

    let mut doc: SiteDocument = serde_json::from_value(raw.clone())
        .map_err(|e| format!("failed to parse SiteDocument IR: {e}"))?;
    registry
        .apply_to_document(&mut doc)
        .map_err(|e| format!("plugin IR transform failed: {e}"))?;
    Ok((doc, "site_ir".to_string()))
}

/// Run full IR, WCAG/compiler, plugin, ABC, and optional `_headers` checks on a JSON value.
#[must_use]
pub fn run_checks_on_value(
    raw: &Value,
    input_label: &str,
    headers_path: Option<&Path>,
    registry: &PluginRegistry,
) -> CheckReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut plugin_findings = Vec::new();
    let mut kind = "partial_json".to_string();

    let plugin_claimed = registry.list().iter().any(|p| p.can_transform_input(raw));
    #[cfg(feature = "pack-hospitality")]
    let heuristic_venue = is_likely_venue_manifest(raw);
    #[cfg(not(feature = "pack-hospitality"))]
    let heuristic_venue = false;
    if plugin_claimed || heuristic_venue {
        kind = "venue_manifest".to_string();
        match registry.transform_input_with(None, raw) {
            Ok(doc) => {
                collect_doc_checks(
                    &doc,
                    Some(raw),
                    registry,
                    &mut errors,
                    &mut warnings,
                    &mut plugin_findings,
                );
            }
            Err(err) => {
                errors.push(CheckIssue {
                    source: err.plugin_id,
                    level: "error".to_string(),
                    code: "manifest_invalid".to_string(),
                    path: "$".to_string(),
                    message: err.message,
                    citation: None,
                });
            }
        }
    } else if let Ok(doc) = serde_json::from_value::<SiteDocument>(raw.clone()) {
        kind = "site_ir".to_string();
        collect_doc_checks(
            &doc,
            Some(raw),
            registry,
            &mut errors,
            &mut warnings,
            &mut plugin_findings,
        );
    } else if is_likely_site_document(raw) {
        kind = "site_ir".to_string();
        if let Err(err) = serde_json::from_value::<SiteDocument>(raw.clone()) {
            errors.push(CheckIssue {
                source: "ir".to_string(),
                level: "error".to_string(),
                code: "ir_parse_error".to_string(),
                path: "$".to_string(),
                message: err.to_string(),
                citation: None,
            });
        }
    }

    // Run hospitality advisory ABC lint directly on raw JSON only when the
    // hospitality pack feature is enabled (covers partial fixtures like `abc-bad.json`).
    #[cfg(feature = "pack-hospitality")]
    {
        let mut seen_plugin_keys: BTreeSet<(String, String, String)> = plugin_findings
            .iter()
            .map(|f| (f.code.clone(), f.path.clone(), f.message.clone()))
            .collect();
        for jf in lint_json_value(raw) {
            let code = jf.finding.rule.to_string();
            let key = (code.clone(), jf.path.clone(), jf.finding.message.clone());
            if seen_plugin_keys.insert(key) {
                plugin_findings.push(CheckIssue {
                    source: "sws-pack-hospitality".to_string(),
                    level: "advisory".to_string(),
                    code,
                    path: jf.path,
                    message: jf.finding.message,
                    citation: Some(ABC_CITATIONS.to_string()),
                });
            }
        }
    }

    if let Some(hpath) = headers_path {
        errors.extend(check_headers_parity(hpath));
    }

    let error_count = errors.len();
    let warning_count = warnings.len();
    let plugin_finding_count = plugin_findings.len();
    let ok = error_count == 0 && plugin_finding_count == 0;

    CheckReport {
        ok,
        input: input_label.to_string(),
        kind,
        errors,
        warnings,
        plugin_findings,
        summary: CheckSummary {
            error_count,
            warning_count,
            plugin_finding_count,
        },
    }
}

fn collect_doc_checks(
    doc: &SiteDocument,
    raw: Option<&Value>,
    registry: &PluginRegistry,
    errors: &mut Vec<CheckIssue>,
    warnings: &mut Vec<CheckIssue>,
    plugin_findings: &mut Vec<CheckIssue>,
) {
    let options = default_compile_options();
    // Pass `None` for registry here so plugin diagnostics are collected separately
    // into `plugin_findings` without duplicating or downgrading advisory items.
    let compiler_diags = check_site_diagnostics(doc, raw, None, &options);
    for diag in compiler_diags {
        let issue = CheckIssue {
            source: "compiler".to_string(),
            level: match diag.level {
                DiagnosticLevel::Error => "error".to_string(),
                DiagnosticLevel::Warning => "warning".to_string(),
            },
            code: diag.code,
            path: diag.path,
            message: diag.message,
            citation: None,
        };
        match diag.level {
            DiagnosticLevel::Error => errors.push(issue),
            DiagnosticLevel::Warning => warnings.push(issue),
        }
    }

    for pd in registry.collect_diagnostics(doc, raw) {
        plugin_findings.push(CheckIssue {
            source: pd.plugin_id,
            level: match pd.severity {
                sws_plugin::DiagnosticSeverity::Error => "error".to_string(),
                sws_plugin::DiagnosticSeverity::Warning => "warning".to_string(),
                sws_plugin::DiagnosticSeverity::Advisory => "advisory".to_string(),
            },
            code: pd.rule,
            path: pd.path,
            message: pd.message,
            citation: pd.citation,
        });
    }
}

#[cfg(feature = "pack-hospitality")]
fn is_likely_venue_manifest(raw: &Value) -> bool {
    let Some(obj) = raw.as_object() else {
        return false;
    };
    obj.contains_key("nap")
        || obj.contains_key("brand")
        || obj.contains_key("hours")
        || obj.contains_key("integrations")
        || obj
            .get("$schema")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("venue"))
}

fn is_likely_site_document(raw: &Value) -> bool {
    let Some(obj) = raw.as_object() else {
        return false;
    };
    obj.contains_key("pages")
        || obj.contains_key("base_url")
        || obj.contains_key("tokens")
        || obj.contains_key("schema_version")
}

/// Verify a `_headers` file's `/*` block matches [`SECURITY_HEADERS`] identically.
#[must_use]
pub fn check_headers_parity(path: &Path) -> Vec<CheckIssue> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(err) => {
            return vec![CheckIssue {
                source: "security".to_string(),
                level: "error".to_string(),
                code: "headers_read_error".to_string(),
                path: path.display().to_string(),
                message: format!("failed to read headers file '{}': {err}", path.display()),
                citation: None,
            }];
        }
    };
    check_headers_parity_str(&content, &path.display().to_string())
}

/// Verify `_headers` text against [`SECURITY_HEADERS`].
#[must_use]
pub fn check_headers_parity_str(content: &str, path_label: &str) -> Vec<CheckIssue> {
    let mut issues = Vec::new();
    let parsed = parse_headers_block(content, "/*");
    if parsed.is_empty() {
        issues.push(CheckIssue {
            source: "security".to_string(),
            level: "error".to_string(),
            code: "headers_parity".to_string(),
            path: path_label.to_string(),
            message: "missing '/*' block or no header entries found in _headers".to_string(),
            citation: None,
        });
        return issues;
    }

    for (name, expected_value) in SECURITY_HEADERS {
        let matches: Vec<&(String, String)> = parsed
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .collect();
        match matches.as_slice() {
            [] => issues.push(CheckIssue {
                source: "security".to_string(),
                level: "error".to_string(),
                code: "headers_parity".to_string(),
                path: format!("{path_label}#{name}"),
                message: format!("missing required security header '{name}'"),
                citation: None,
            }),
            [(actual_name, actual_value)] => {
                if actual_value != expected_value {
                    issues.push(CheckIssue {
                        source: "security".to_string(),
                        level: "error".to_string(),
                        code: "headers_parity".to_string(),
                        path: format!("{path_label}#{actual_name}"),
                        message: format!(
                            "header '{actual_name}' mismatch: expected '{expected_value}', got '{actual_value}'"
                        ),
                        citation: None,
                    });
                }
            }
            _ => issues.push(CheckIssue {
                source: "security".to_string(),
                level: "error".to_string(),
                code: "headers_parity".to_string(),
                path: format!("{path_label}#{name}"),
                message: format!("duplicate security header '{name}' in '/*' block"),
                citation: None,
            }),
        }
    }

    for (actual_name, _) in &parsed {
        if !SECURITY_HEADERS
            .iter()
            .any(|(expected_name, _)| expected_name.eq_ignore_ascii_case(actual_name))
        {
            issues.push(CheckIssue {
                source: "security".to_string(),
                level: "error".to_string(),
                code: "headers_parity".to_string(),
                path: format!("{path_label}#{actual_name}"),
                message: format!("unexpected header '{actual_name}' in '/*' block"),
                citation: None,
            });
        }
    }

    issues
}

fn report_compiler_diagnostics(compiled: &CompiledSite) -> Result<(), String> {
    let mut error_count = 0usize;
    for diag in &compiled.diagnostics {
        match diag.level {
            DiagnosticLevel::Error => {
                error_count += 1;
                eprintln!("error [{}] {}: {}", diag.code, diag.path, diag.message);
            }
            DiagnosticLevel::Warning => {
                eprintln!("warning [{}] {}: {}", diag.code, diag.path, diag.message);
            }
        }
    }
    if error_count > 0 {
        return Err(format!("compiler reported {error_count} error(s)"));
    }
    Ok(())
}

fn safe_relative_path(rel_path: &str) -> Result<PathBuf, String> {
    let trimmed = rel_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err("compiled output path must not be empty".to_string());
    }
    let path = Path::new(trimmed);
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("unsafe compiled output path '{rel_path}'"));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(format!("invalid compiled output path '{rel_path}'"));
    }
    Ok(clean)
}

/// Write compiled files deterministically into `outdir`.
pub fn write_compiled_files(outdir: &Path, files: &BTreeMap<String, String>) -> Result<(), String> {
    fs::create_dir_all(outdir)
        .map_err(|e| format!("failed to create directory '{}': {e}", outdir.display()))?;

    for (rel_path, contents) in files {
        let safe_rel = safe_relative_path(rel_path)?;
        let full_path = outdir.join(safe_rel);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                format!("failed to create directory '{}': {e}", parent.display())
            })?;
        }
        fs::write(&full_path, contents.as_bytes())
            .map_err(|e| format!("failed to write '{}': {e}", full_path.display()))?;
    }
    Ok(())
}

/// Compare existing files in `outdir` byte-for-byte against `files`.
pub fn check_compiled_files(
    outdir: &Path,
    files: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let mut diffs = Vec::new();
    for (rel_path, expected) in files {
        let safe_rel = safe_relative_path(rel_path)?;
        let full_path = outdir.join(&safe_rel);
        match fs::read(&full_path) {
            Ok(actual) => {
                if actual != expected.as_bytes() {
                    diffs.push(format!(
                        "{} differs (expected {} B, found {} B)",
                        safe_rel.display(),
                        expected.len(),
                        actual.len()
                    ));
                }
            }
            Err(_) => {
                diffs.push(format!("{} is missing", safe_rel.display()));
            }
        }
    }
    Ok(diffs)
}

fn print_human_check_report(report: &CheckReport) {
    for issue in &report.errors {
        eprintln!(
            "error [{} / {}] {}: {}",
            issue.source, issue.code, issue.path, issue.message
        );
    }
    for issue in &report.warnings {
        eprintln!(
            "warning [{} / {}] {}: {}",
            issue.source, issue.code, issue.path, issue.message
        );
    }
    for issue in &report.plugin_findings {
        eprintln!(
            "finding [{} / {}] {}: {}",
            issue.source, issue.code, issue.path, issue.message
        );
    }
    println!(
        "solo check ({}): {} error(s), {} warning(s), {} plugin finding(s) -> {}",
        report.kind,
        report.summary.error_count,
        report.summary.warning_count,
        report.summary.plugin_finding_count,
        if report.ok { "PASS" } else { "FAIL" }
    );
}
