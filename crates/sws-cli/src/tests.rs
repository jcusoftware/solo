//! Unit and integration tests for `sws-cli` and the stdio MCP server.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use sws_ir::{SiteDocument, validate_ir};
#[cfg(feature = "pack-hospitality")]
use sws_pack_hospitality::abc::lint_json_value;
#[cfg(feature = "pack-hospitality")]
use sws_pack_hospitality::venue::{VenueManifest, validate_venue};
#[cfg(feature = "pack-hospitality")]
use sws_security::headers::render_headers_file;
use sws_security::headers::SECURITY_HEADERS;

use crate::commands::{
    CheckReport, cmd_build, cmd_check, cmd_export, cmd_init, default_registry, run_checks_on_value,
};
use crate::mcp::serve_mcp;
use crate::templates::{MIGRATION_0001_INIT_SQL, starter_site_document};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sws-cli-test-{label}-{pid}-{id}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn init_generic_creates_valid_accessible_site_ir() {
    let dir = TempDir::new("init-generic");
    let target = dir.path().join("site");
    cmd_init(&target, None).expect("solo init should succeed");

    let ir_path = target.join("site.ir.json");
    let readme_path = target.join("README.md");
    assert!(ir_path.exists());
    assert!(readme_path.exists());

    let raw_str = fs::read_to_string(&ir_path).expect("read site.ir.json");
    let doc: SiteDocument = serde_json::from_str(&raw_str).expect("parse SiteDocument");
    assert!(
        validate_ir(&doc).is_empty(),
        "starter IR must pass validate_ir: {:?}",
        validate_ir(&doc)
    );
    assert!(cmd_check(&ir_path, None, false).expect("check runs"));

    // Core SWS generic starter must stay free of hospitality/Virginia references.
    let lower = raw_str.to_ascii_lowercase();
    assert!(!lower.contains("virginia"));
    assert!(!lower.contains("shirlington"));
    assert!(!lower.contains("hospitality"));
}

#[cfg(feature = "pack-hospitality")]
#[test]
fn init_hospitality_creates_valid_fictional_venue() {
    let dir = TempDir::new("init-hospitality");
    let target = dir.path().join("venue");
    cmd_init(&target, Some("hospitality")).expect("solo init --pack hospitality should succeed");

    let venue_path = target.join("venue.json");
    let readme_path = target.join("README.md");
    assert!(venue_path.exists());
    assert!(readme_path.exists());

    let raw_str = fs::read_to_string(&venue_path).expect("read venue.json");
    let venue: VenueManifest = serde_json::from_str(&raw_str).expect("parse VenueManifest");
    let errors = validate_venue(&venue);
    assert!(errors.is_empty(), "starter venue errors: {errors:?}");
    assert!(venue.nap.website.ends_with(".example"));
    assert!(venue.nap.email.ends_with(".example"));
    assert!(venue.nap.phone.contains("555-01"));

    let raw_val: Value = serde_json::from_str(&raw_str).expect("parse JSON");
    assert!(lint_json_value(&raw_val).is_empty());
    assert!(cmd_check(&venue_path, None, false).expect("check passes"));
}

#[cfg(not(feature = "pack-hospitality"))]
#[test]
fn core_cli_without_pack_feature_stays_domain_agnostic() {
    assert!(default_registry().list().is_empty());
    let dir = TempDir::new("init-no-pack");
    let err = cmd_init(dir.path(), Some("hospitality")).unwrap_err();
    assert!(err.contains("pack-hospitality"), "{err}");
}

#[test]
fn init_rejects_unknown_pack() {
    let dir = TempDir::new("init-bad-pack");
    let err = cmd_init(dir.path(), Some("unknown-pack")).unwrap_err();
    assert!(err.contains("unsupported pack"));
}

#[test]
fn build_and_build_check_round_trip_and_detect_staleness() {
    let dir = TempDir::new("build-check");
    let site_dir = dir.path().join("src");
    let out_dir = dir.path().join("public");
    cmd_init(&site_dir, None).expect("init");
    let ir_path = site_dir.join("site.ir.json");

    cmd_build(&ir_path, &out_dir, false).expect("initial build");
    assert!(out_dir.join("index.html").exists());
    assert!(out_dir.join("404.html").exists());
    assert!(out_dir.join("styles.css").exists());
    assert!(out_dir.join("robots.txt").exists());
    assert!(out_dir.join("sitemap.xml").exists());
    assert!(out_dir.join("_headers").exists());
    assert!(out_dir.join("js/forms.js").exists());

    // Exact match passes `--check`.
    cmd_build(&ir_path, &out_dir, true).expect("build --check passes");

    // Mutating output fails `--check`.
    let index_path = out_dir.join("index.html");
    let original_index = fs::read_to_string(&index_path).expect("read index.html");
    fs::write(&index_path, format!("{original_index}\n<!-- stale -->")).expect("tamper");
    let stale_err = cmd_build(&ir_path, &out_dir, true).unwrap_err();
    assert!(stale_err.contains("missing or out of date"));

    // Deleting a file also fails `--check`.
    fs::remove_file(&index_path).expect("remove index.html");
    let missing_err = cmd_build(&ir_path, &out_dir, true).unwrap_err();
    assert!(missing_err.contains("missing or out of date"));
}

#[cfg(feature = "pack-hospitality")]
#[test]
fn build_compiles_hospitality_demo_manifest() {
    let dir = TempDir::new("build-demo-venue");
    let out_dir = dir.path().join("public");
    let demo_venue = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packs/hospitality/demo/venue.json");

    cmd_build(&demo_venue, &out_dir, false).expect("build demo venue");
    assert!(out_dir.join("index.html").exists());
    assert!(out_dir.join("styles.css").exists());
    cmd_build(&demo_venue, &out_dir, true).expect("build --check demo venue");
}

#[cfg(feature = "pack-hospitality")]
#[test]
fn check_validates_demo_venue_headers_and_abc_bad_fixture() {
    let dir = TempDir::new("check");
    let demo_venue = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packs/hospitality/demo/venue.json");
    let abc_bad = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core/tools/fixtures/abc-bad.json");

    let headers_ok = dir.path().join("_headers");
    fs::write(&headers_ok, render_headers_file()).expect("write _headers");
    assert!(
        cmd_check(&demo_venue, Some(&headers_ok), false).expect("check demo venue + headers"),
        "clean demo venue with matching _headers must pass"
    );

    // Tampered _headers must fail.
    let headers_bad = dir.path().join("_headers.bad");
    fs::write(&headers_bad, "/*\n  X-Frame-Options: SAMEORIGIN\n").expect("write bad headers");
    assert!(
        !cmd_check(&demo_venue, Some(&headers_bad), false).expect("check runs"),
        "mismatched _headers must fail"
    );

    // Partial fixture `abc-bad.json` must fail with ABC findings.
    let raw_bad: Value =
        serde_json::from_str(&fs::read_to_string(&abc_bad).expect("read abc-bad.json"))
            .expect("parse abc-bad.json");
    let report = run_checks_on_value(&raw_bad, "abc-bad.json", None, &default_registry());
    assert!(!report.ok);
    assert_eq!(report.kind, "partial_json");
    assert!(report.errors.is_empty(), "partial fixture should not emit IR parse errors: {:?}", report.errors);
    assert!(
        report.plugin_findings.iter().any(|f| f.code == "bottomless"),
        "expected bottomless finding: {:?}",
        report.plugin_findings
    );
    assert!(
        report
            .plugin_findings
            .iter()
            .any(|f| f.code == "happy_hour_late_end"),
        "expected happy_hour_late_end finding"
    );
    assert!(!cmd_check(&abc_bad, None, true).expect("check --json runs"));
}

#[test]
fn check_rejects_broken_site_ir() {
    let mut doc = starter_site_document();
    doc.slug = "INVALID_SLUG!".to_string();
    let val = serde_json::to_value(&doc).expect("serialize");
    let report = run_checks_on_value(&val, "broken.ir.json", None, &default_registry());
    assert!(!report.ok);
    assert!(report.summary.error_count > 0);
}

#[test]
fn starter_site_uses_forms_contact_js_island_and_baseline_styles() {
    let dir = TempDir::new("starter-site");
    let site_dir = dir.path().join("src");
    let out_dir = dir.path().join("public");
    cmd_init(&site_dir, None).expect("init generic");
    let ir_path = site_dir.join("site.ir.json");
    cmd_build(&ir_path, &out_dir, false).expect("build starter");

    let html = fs::read_to_string(out_dir.join("index.html")).expect("read index.html");
    assert!(html.contains("action=\"/api/forms/contact\""), "{html}");
    assert!(html.contains("js-api-form"), "{html}");
    assert!(!html.contains("/api/inquire"), "{html}");

    let css = fs::read_to_string(out_dir.join("styles.css")).expect("read styles.css");
    assert!(css.contains(".skip-link"), "{css}");
    assert!(css.contains(":focus-visible"), "{css}");
}

#[test]
fn export_emits_standalone_rust_worker_wrangler_and_d1_migration() {
    let dir = TempDir::new("export");
    let src_dir = dir.path().join("src-site");
    let out_dir = dir.path().join("dist");
    cmd_init(&src_dir, None).expect("init generic");
    let ir_path = src_dir.join("site.ir.json");

    cmd_export(&ir_path, &out_dir).expect("export starter site");

    assert!(out_dir.join("public/index.html").exists());
    assert!(out_dir.join("public/styles.css").exists());
    assert!(out_dir.join("public/_headers").exists());
    assert!(out_dir.join(".dev.vars.example").exists());
    assert!(out_dir.join("README.md").exists());

    let cargo_toml = fs::read_to_string(out_dir.join("Cargo.toml")).expect("read Cargo.toml");
    assert!(cargo_toml.contains("crate-type = [\"cdylib\", \"rlib\"]"));
    assert!(cargo_toml.contains("worker = { version = \"0.9\""));

    let worker_lib = fs::read_to_string(out_dir.join("src/lib.rs")).expect("read src/lib.rs");
    for (name, value) in SECURITY_HEADERS {
        assert!(worker_lib.contains(name), "missing header {name} in src/lib.rs");
        assert!(worker_lib.contains(value), "missing header value for {name} in src/lib.rs");
    }
    assert!(worker_lib.contains("INSERT INTO form_submissions"));
    assert!(worker_lib.contains("INSERT INTO newsletter_subscribers"));
    assert!(worker_lib.contains("form_encode(&pairs)"));
    assert!(worker_lib.contains("ReadableStreamDefaultReader"));
    assert!(!worker_lib.contains("req.bytes()"));
    assert!(worker_lib.contains("\"/api/inquire\""));
    assert!(worker_lib.contains("consent_timestamp"));
    assert!(!worker_lib.contains("1970-01-01T00:00:00Z\""));

    let dev_vars_example =
        fs::read_to_string(out_dir.join(".dev.vars.example")).expect("read .dev.vars.example");
    assert!(dev_vars_example.contains("# DEV_MODE=true"));
    assert!(!dev_vars_example.lines().any(|l| l.trim() == "DEV_MODE=true"));

    let wrangler = fs::read_to_string(out_dir.join("wrangler.jsonc")).expect("read wrangler.jsonc");
    assert!(wrangler.contains("\"starter-site-demo\""), "{wrangler}");
    assert!(wrangler.contains("\"main\": \"build/index.js\""));
    assert!(wrangler.contains("worker-build --release"));
    assert!(wrangler.contains("\"./public\""));
    assert!(wrangler.contains("\"migrations\""));

    let sql = fs::read_to_string(out_dir.join("migrations/0001_init.sql")).expect("read sql");
    assert_eq!(sql, MIGRATION_0001_INIT_SQL);
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS form_submissions"));
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS newsletter_subscribers"));
}

#[test]
fn mcp_stdio_round_trip_supports_all_methods_and_tools() {
    let dir = TempDir::new("mcp");
    let starter = starter_site_document();
    let starter_path = dir.path().join("site.ir.json");
    fs::write(
        &starter_path,
        serde_json::to_string_pretty(&starter).expect("serialize starter"),
    )
    .expect("write starter");

    let patch_req = json!({
        "op": "set_site_meta",
        "name": "Patched Starter Site"
    });

    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "0.1.0" }
            }
        }),
        // Notification: must produce NO response line.
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "read_site_ir",
                "arguments": { "path": "site.ir.json" }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "apply_ir_patch",
                "arguments": {
                    "ir_json": starter,
                    "patch_json": patch_req
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "compile_preview",
                "arguments": { "ir_json": starter }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "run_a11y_and_plugin_checks",
                "arguments": {
                    "input_json": {
                        "schema_version": 1,
                        "slug": "INVALID_SLUG!",
                        "name": "Broken",
                        "base_url": "https://broken.example",
                        "pages": []
                    }
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "nonexistent_tool",
                "arguments": {}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "unknown/method"
        }),
    ];

    let input_lines = requests
        .iter()
        .map(|v| serde_json::to_string(v).expect("serialize req"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n{invalid json\n";

    let mut output_buf = Vec::new();
    serve_mcp(Cursor::new(input_lines.as_bytes()), &mut output_buf, dir.path())
        .expect("serve_mcp");

    let output_str = String::from_utf8(output_buf).expect("utf8");
    let responses: Vec<Value> = output_str
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid json response"))
        .collect();

    // 9 requests minus 1 notification + 1 invalid JSON line = 9 responses.
    assert_eq!(responses.len(), 9, "responses: {responses:#?}");

    // 1. initialize
    assert_eq!(responses[0]["id"], json!(1));
    assert_eq!(responses[0]["result"]["protocolVersion"], json!("2024-11-05"));
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], json!("solo-mcp"));

    // 2. tools/list (note id=2 because notification produced no response)
    assert_eq!(responses[1]["id"], json!(2));
    let tools = responses[1]["result"]["tools"].as_array().expect("tools array");
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        tool_names,
        vec![
            "read_site_ir",
            "apply_ir_patch",
            "compile_preview",
            "run_a11y_and_plugin_checks"
        ]
    );

    // 3. read_site_ir on jailed starter site.ir.json
    assert_eq!(responses[2]["id"], json!(3));
    assert_eq!(responses[2]["result"]["isError"], json!(false));
    let read_text = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    let read_doc: SiteDocument = serde_json::from_str(read_text).expect("SiteDocument");
    assert_eq!(read_doc.slug, "starter-site-demo");

    // 4. apply_ir_patch
    assert_eq!(responses[3]["id"], json!(4));
    assert_eq!(responses[3]["result"]["isError"], json!(false));
    let patched_text = responses[3]["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    let patched_doc: SiteDocument = serde_json::from_str(patched_text).expect("SiteDocument");
    assert_eq!(patched_doc.name, "Patched Starter Site");

    // 5. compile_preview
    assert_eq!(responses[4]["id"], json!(5));
    assert_eq!(responses[4]["result"]["isError"], json!(false));
    let preview_text = responses[4]["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    let preview: Value = serde_json::from_str(preview_text).expect("preview json");
    assert!(preview["files"]["index.html"].is_string());
    assert!(preview["files"]["styles.css"].is_string());
    assert!(preview["index_html_bytes"].as_u64().unwrap_or(0) > 0);
    assert!(preview["styles_css_bytes"].as_u64().unwrap_or(0) > 0);

    // 6. run_a11y_and_plugin_checks
    assert_eq!(responses[5]["id"], json!(6));
    assert_eq!(responses[5]["result"]["isError"], json!(false));
    let check_text = responses[5]["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    let check_report: CheckReport = serde_json::from_str(check_text).expect("CheckReport");
    assert!(!check_report.ok);
    assert!(check_report.summary.error_count > 0);

    // 7. unknown tool -> -32602
    assert_eq!(responses[6]["id"], json!(7));
    assert_eq!(responses[6]["error"]["code"], json!(-32602));

    // 8. unknown method -> -32601
    assert_eq!(responses[7]["id"], json!(8));
    assert_eq!(responses[7]["error"]["code"], json!(-32601));

    // 9. parse error -> -32700
    assert_eq!(responses[8]["id"], Value::Null);
    assert_eq!(responses[8]["error"]["code"], json!(-32700));
}

#[test]
fn mcp_read_site_ir_jails_paths_and_blocks_sensitive_files() {
    let dir = TempDir::new("mcp-jail");
    let root = dir.path().join("workspace");
    let outside = dir.path().join("outside.json");
    fs::create_dir_all(root.join("private")).expect("create private dir");
    fs::write(&outside, "{}").expect("write outside");
    fs::write(root.join(".dev.vars"), "TURNSTILE_SECRET_KEY=secret").expect("write .dev.vars");
    fs::write(root.join(".env"), "API_KEY=secret").expect("write .env");
    fs::write(root.join("secrets.env"), "SECRET=1").expect("write secrets.env");
    fs::write(root.join("private/dossier.json"), "{}").expect("write dossier");

    let blocked_paths = [
        "../outside.json",
        outside.to_str().expect("utf8 path"),
        ".dev.vars",
        ".env",
        "secrets.env",
        "private/dossier.json",
    ];

    for (idx, p) in blocked_paths.iter().enumerate() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": idx + 1,
            "method": "tools/call",
            "params": {
                "name": "read_site_ir",
                "arguments": { "path": p }
            }
        });
        let line = format!("{req}\n");
        let mut out = Vec::new();
        serve_mcp(Cursor::new(line.as_bytes()), &mut out, &root).expect("serve_mcp");
        let resp: Value =
            serde_json::from_slice(&out).expect("parse mcp response");
        assert_eq!(
            resp["result"]["isError"],
            json!(true),
            "expected blocked path '{p}' to error, got: {resp}"
        );
    }
}
