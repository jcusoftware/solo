//! `solo studio` subcommands: `login`, `pull`, `push`, `publish`, `status`, `enable-r2` (#68).
//!
//! All HTTP/TLS uses the native blocking client in [`crate::http_client`]
//! (no Node.js, no `wrangler`, no `curl`). Credentials are stored outside the
//! workspace with mode `0600`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sws_assets::hash::sha256_hex;
use sws_compiler::{CompileOptions, DiagnosticLevel, compile_from_json};

use crate::commands::{default_registry, run_checks_on_value};
use crate::http_client;

#[derive(Debug, Subcommand)]
pub enum StudioAction {
    /// Save Studio URL and bearer token to user config (mode 0600, outside repo).
    Login {
        /// Studio base URL (e.g. https://studio.example or http://127.0.0.1:8788).
        #[arg(long)]
        url: String,
        /// Read the bearer token from standard input (otherwise uses SOLO_STUDIO_TOKEN).
        #[arg(long)]
        token_stdin: bool,
    },
    /// Download the current site IR document and revision metadata from Studio.
    Pull {
        /// Target site as <workspace_id>/<site_id>.
        #[arg(long)]
        site: String,
        /// Output path for the pulled SiteDocument IR JSON.
        #[arg(long)]
        out: PathBuf,
        /// Optional path for the sync metadata sidecar (defaults to <out>.studio-sync.json).
        #[arg(long)]
        state: Option<PathBuf>,
        /// Optional sequence number for incremental replay.
        #[arg(long)]
        since_seq: Option<i64>,
        /// Override Studio base URL.
        #[arg(long)]
        url: Option<String>,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Push a command batch, transaction, initial seed, or checkpoint to Studio.
    Push {
        /// Target site as <workspace_id>/<site_id>.
        #[arg(long)]
        site: String,
        /// Command batch JSON file ({ "version": 1, "commands": [...] }).
        #[arg(long)]
        batch: Option<PathBuf>,
        /// Transaction JSON file from `solo edit apply --transaction` (pushes its forward commands).
        #[arg(long)]
        transaction: Option<PathBuf>,
        /// Initial SiteDocument IR JSON file to seed an empty site (requires publish scope).
        #[arg(long)]
        seed: Option<PathBuf>,
        /// Create a checkpoint ("save" or "publish").
        #[arg(long)]
        checkpoint: Option<String>,
        /// Base sequence number for batch (defaults to sidecar state if present).
        #[arg(long)]
        base_seq: Option<i64>,
        /// Idempotency operation key (8-64 chars of [A-Za-z0-9._:-]).
        #[arg(long)]
        op_id: Option<String>,
        /// Sync metadata sidecar path.
        #[arg(long)]
        state: Option<PathBuf>,
        /// Override Studio base URL.
        #[arg(long)]
        url: Option<String>,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Verify + compile locally, submit a revision-pinned manifest (origin: cli), and optionally upload/wait/activate.
    Publish {
        /// Target site as <workspace_id>/<site_id>.
        #[arg(long)]
        site: String,
        /// Local SiteDocument IR or venue.json to verify and compile (if omitted, pulls current Studio IR).
        #[arg(long)]
        input: Option<PathBuf>,
        /// Pin to an explicit revision_id (if omitted, creates/reuses a publish checkpoint).
        #[arg(long)]
        revision_id: Option<String>,
        /// Pin to an explicit revision_sha256 (required when --revision-id is set).
        #[arg(long)]
        revision_sha256: Option<String>,
        /// Idempotency request_id (auto-generated if omitted).
        #[arg(long)]
        request_id: Option<String>,
        /// Attempt to upload the compiled bundle to PUT .../publish/bundles/{manifest_sha256}.
        #[arg(long)]
        bundle: bool,
        /// Poll publish job status until terminal (ready or failed).
        #[arg(long)]
        wait: bool,
        /// Attempt to activate the ready deployment (if server endpoint is available).
        #[arg(long)]
        activate: bool,
        /// Override Studio base URL.
        #[arg(long)]
        url: Option<String>,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Inspect a Studio publish job status (distinguishing origin: cli vs server).
    Status {
        /// Target site as <workspace_id>/<site_id>.
        #[arg(long)]
        site: String,
        /// Publish job id.
        #[arg(long)]
        job: String,
        /// Override Studio base URL.
        #[arg(long)]
        url: Option<String>,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Print exact wrangler commands, wrangler.studio.jsonc diff, and quotas for opt-in R2 media storage.
    EnableR2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StudioConfig {
    url: String,
    token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SyncSidecar {
    workspace_id: String,
    site_id: String,
    head_seq: i64,
    #[serde(default)]
    checkpoint_seq: Option<i64>,
    #[serde(default)]
    revision_id: Option<String>,
    #[serde(default)]
    ir_sha256: Option<String>,
}

fn is_valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn parse_site_ref(site_ref: &str) -> Result<(String, String), String> {
    let (ws, site) = site_ref
        .split_once('/')
        .ok_or_else(|| format!("--site must be <workspace_id>/<site_id>, got '{site_ref}'"))?;
    if !is_valid_id(ws) || !is_valid_id(site) {
        return Err(format!(
            "invalid workspace or site id in '{site_ref}' (expected [a-z0-9_-]{{1,64}})"
        ));
    }
    Ok((ws.to_string(), site.to_string()))
}

fn config_path() -> Result<PathBuf, String> {
    if let Ok(custom) = std::env::var("SOLO_STUDIO_CONFIG")
        && !custom.trim().is_empty()
    {
        return Ok(PathBuf::from(custom));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.trim().is_empty()
    {
        return Ok(PathBuf::from(xdg).join("solo").join("studio.json"));
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| "cannot determine home directory for Studio config".to_string())?;
    Ok(PathBuf::from(home).join(".config").join("solo").join("studio.json"))
}

fn write_secret_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create config dir '{}': {e}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("failed to chmod config dir '{}': {e}", parent.display()))?;
        }
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map_or_else(|| "studio.json".into(), |n| n.to_string_lossy().into_owned());
    let temp = dir.join(format!(".{name}.tmp-{}", std::process::id()));

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| format!("failed to create '{}': {e}", temp.display()))?
    };
    #[cfg(not(unix))]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("failed to create '{}': {e}", temp.display()))?;

    if let Err(e) = file.write_all(content.as_bytes()).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(format!("failed to write '{}': {e}", path.display()));
    }
    fs::rename(&temp, path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("failed to save '{}': {e}", path.display())
    })?;
    Ok(())
}

fn write_atomic_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory '{}': {e}", parent.display()))?;
    }
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map_or_else(|| "out".into(), |n| n.to_string_lossy().into_owned());
    let temp = dir.join(format!(".{name}.solo-tmp-{}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("failed to create temp file '{}': {e}", temp.display()))?;
    if let Err(e) = file.write_all(content.as_bytes()).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(format!("failed to write '{}': {e}", path.display()));
    }
    fs::rename(&temp, path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("failed to write '{}': {e}", path.display())
    })
}

fn resolve_auth(url_override: Option<&str>) -> Result<(String, String), String> {
    let cfg = config_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<StudioConfig>(&s).ok());

    let url = url_override
        .map(str::to_string)
        .or_else(|| std::env::var("SOLO_STUDIO_URL").ok().filter(|s| !s.trim().is_empty()))
        .or_else(|| cfg.as_ref().map(|c| c.url.clone()))
        .ok_or_else(|| {
            "Studio URL not configured; pass --url or run `solo studio login --url <url>`".to_string()
        })?;
    let url = url.trim().trim_end_matches('/').to_string();
    http_client::parse_url(&url)?;

    let token = std::env::var("SOLO_STUDIO_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| cfg.map(|c| c.token))
        .ok_or_else(|| {
            "Studio token not configured; set SOLO_STUDIO_TOKEN or run `solo studio login`".to_string()
        })?;

    Ok((url, token))
}

fn sidecar_path_for(target: &Path, explicit: Option<&Path>) -> PathBuf {
    match explicit {
        Some(p) => p.to_path_buf(),
        None => {
            let mut os = target.as_os_str().to_os_string();
            os.push(".studio-sync.json");
            PathBuf::from(os)
        }
    }
}

fn gen_id(prefix: &str, seed: &[u8]) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut buf = format!("{prefix}\0{nanos}\0{}\0", std::process::id()).into_bytes();
    buf.extend_from_slice(seed);
    let hex = sha256_hex(&buf);
    format!("{prefix}-{}", &hex[..24])
}

fn parse_json_response(resp: &http_client::HttpResponse) -> Value {
    serde_json::from_slice(&resp.body).unwrap_or_else(|_| {
        json!({
            "raw": String::from_utf8_lossy(&resp.body).into_owned()
        })
    })
}

fn format_api_error(context: &str, resp: &http_client::HttpResponse) -> String {
    let body = parse_json_response(resp);
    let code = body
        .get("code")
        .or_else(|| body.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("unknown_error");
    let msg = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("");
    if resp.status == 409 && body.get("type").and_then(Value::as_str) == Some("conflict") {
        let kind = body.get("kind").and_then(Value::as_str).unwrap_or("conflict");
        let head = body.get("head_seq").and_then(Value::as_i64).unwrap_or(-1);
        let paths = body
            .get("paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        return format!(
            "{context}: HTTP 409 conflict ({kind}, head_seq={head}): {msg}{}",
            if paths.is_empty() {
                String::new()
            } else {
                format!(" [paths: {paths}]")
            }
        );
    }
    if msg.is_empty() {
        format!("{context}: HTTP {} ({code})", resp.status)
    } else {
        format!("{context}: HTTP {} ({code}): {msg}", resp.status)
    }
}

pub fn run(action: StudioAction) -> Result<(), String> {
    match action {
        StudioAction::Login { url, token_stdin } => cmd_login(&url, token_stdin),
        StudioAction::Pull {
            site,
            out,
            state,
            since_seq,
            url,
            json,
        } => cmd_pull(&site, &out, state.as_deref(), since_seq, url.as_deref(), json),
        StudioAction::Push {
            site,
            batch,
            transaction,
            seed,
            checkpoint,
            base_seq,
            op_id,
            state,
            url,
            json,
        } => cmd_push(
            &site,
            batch.as_deref(),
            transaction.as_deref(),
            seed.as_deref(),
            checkpoint.as_deref(),
            base_seq,
            op_id.as_deref(),
            state.as_deref(),
            url.as_deref(),
            json,
        ),
        StudioAction::Publish {
            site,
            input,
            revision_id,
            revision_sha256,
            request_id,
            bundle,
            wait,
            activate,
            url,
            json,
        } => cmd_publish(
            &site,
            input.as_deref(),
            revision_id.as_deref(),
            revision_sha256.as_deref(),
            request_id.as_deref(),
            bundle,
            wait,
            activate,
            url.as_deref(),
            json,
        ),
        StudioAction::Status {
            site,
            job,
            url,
            json,
        } => cmd_status(&site, &job, url.as_deref(), json),
        StudioAction::EnableR2 => {
            cmd_enable_r2();
            Ok(())
        }
    }
}

fn cmd_login(url: &str, token_stdin: bool) -> Result<(), String> {
    let clean_url = url.trim().trim_end_matches('/').to_string();
    http_client::parse_url(&clean_url)?;

    let token = if token_stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("failed to read token from stdin: {e}"))?;
        buf.trim().to_string()
    } else {
        std::env::var("SOLO_STUDIO_TOKEN")
            .map_err(|_| "pass --token-stdin or set SOLO_STUDIO_TOKEN".to_string())?
            .trim()
            .to_string()
    };
    if token.is_empty() {
        return Err("Studio token must not be empty".to_string());
    }

    let path = config_path()?;
    let cfg = StudioConfig {
        url: clean_url.clone(),
        token,
    };
    let text = serde_json::to_string_pretty(&cfg)
        .map_err(|e| format!("failed to serialize Studio config: {e}"))?
        + "\n";
    write_secret_file(&path, &text)?;
    println!("solo studio login: saved credentials for {clean_url} to {}", path.display());
    Ok(())
}

fn cmd_pull(
    site_ref: &str,
    out: &Path,
    state_path: Option<&Path>,
    since_seq: Option<i64>,
    url_override: Option<&str>,
    json_out: bool,
) -> Result<(), String> {
    let (ws, site) = parse_site_ref(site_ref)?;
    let (base_url, token) = resolve_auth(url_override)?;
    let mut endpoint = format!("{base_url}/api/sites/{ws}/{site}/sync");
    if let Some(seq) = since_seq {
        endpoint.push_str(&format!("?since_seq={seq}"));
    }
    let resp = http_client::request("GET", &endpoint, Some(&token), None, None)?;
    if resp.status != 200 {
        return Err(format_api_error("solo studio pull", &resp));
    }
    let body = parse_json_response(&resp);
    let kind = body.get("type").and_then(Value::as_str).unwrap_or("");
    let head_seq = body.get("head_seq").and_then(Value::as_i64).unwrap_or(0);
    let checkpoint_seq = body.get("checkpoint_seq").and_then(Value::as_i64);
    let revision_id = body
        .get("revision_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    if kind == "state" {
        let doc = body
            .get("document")
            .and_then(Value::as_str)
            .ok_or_else(|| "Studio sync state response missing 'document'".to_string())?;
        write_atomic_file(out, doc)?;
        let ir_sha256 = body
            .get("ir_sha256")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| sha256_hex(doc.as_bytes()));
        let sidecar = SyncSidecar {
            workspace_id: ws.clone(),
            site_id: site.clone(),
            head_seq,
            checkpoint_seq,
            revision_id: revision_id.clone(),
            ir_sha256: Some(ir_sha256.clone()),
        };
        let sc_path = sidecar_path_for(out, state_path);
        let sc_text = serde_json::to_string_pretty(&sidecar)
            .map_err(|e| format!("failed to serialize sync state: {e}"))?
            + "\n";
        write_atomic_file(&sc_path, &sc_text)?;
        if json_out {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "type": "state",
                    "workspace_id": ws,
                    "site_id": site,
                    "head_seq": head_seq,
                    "checkpoint_seq": checkpoint_seq,
                    "revision_id": revision_id,
                    "ir_sha256": ir_sha256,
                    "out": out,
                }))
                .unwrap_or_default()
            );
        } else {
            println!(
                "solo studio pull: {ws}/{site} head_seq={head_seq} ir_sha256={ir_sha256} wrote '{}'",
                out.display()
            );
        }
        Ok(())
    } else if kind == "replay" {
        if json_out {
            println!(
                "{}",
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );
        } else {
            let count = body
                .get("entries")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            println!("solo studio pull: {ws}/{site} head_seq={head_seq} replayed {count} entries");
        }
        Ok(())
    } else {
        Err(format!("unexpected sync response type '{kind}'"))
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_push(
    site_ref: &str,
    batch_path: Option<&Path>,
    tx_path: Option<&Path>,
    seed_path: Option<&Path>,
    checkpoint_reason: Option<&str>,
    base_seq: Option<i64>,
    op_id: Option<&str>,
    state_path: Option<&Path>,
    url_override: Option<&str>,
    json_out: bool,
) -> Result<(), String> {
    let mode_count = usize::from(batch_path.is_some())
        + usize::from(tx_path.is_some())
        + usize::from(seed_path.is_some())
        + usize::from(checkpoint_reason.is_some());
    if mode_count != 1 {
        return Err(
            "specify exactly one of --batch, --transaction, --seed, or --checkpoint".to_string(),
        );
    }
    let (ws, site) = parse_site_ref(site_ref)?;
    let (base_url, token) = resolve_auth(url_override)?;
    let endpoint = format!("{base_url}/api/sites/{ws}/{site}/sync");

    let payload = if let Some(path) = seed_path {
        let doc_str = fs::read_to_string(path)
            .map_err(|e| format!("failed to read seed file '{}': {e}", path.display()))?;
        json!({ "v": 1, "type": "seed", "document": doc_str })
    } else if let Some(reason) = checkpoint_reason {
        if reason != "save" && reason != "publish" {
            return Err("--checkpoint must be 'save' or 'publish'".to_string());
        }
        json!({ "v": 1, "type": "checkpoint", "reason": reason })
    } else {
        let (batch_val, ref_path) = if let Some(bp) = batch_path {
            let text = fs::read_to_string(bp)
                .map_err(|e| format!("failed to read batch '{}': {e}", bp.display()))?;
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| format!("invalid JSON in '{}': {e}", bp.display()))?;
            (v, bp)
        } else {
            let tp = tx_path.expect("checked mode_count == 1");
            let text = fs::read_to_string(tp)
                .map_err(|e| format!("failed to read transaction '{}': {e}", tp.display()))?;
            let tx: Value = serde_json::from_str(&text)
                .map_err(|e| format!("invalid JSON in '{}': {e}", tp.display()))?;
            let forward = tx
                .get("forward")
                .cloned()
                .ok_or_else(|| format!("transaction '{}' missing 'forward' array", tp.display()))?;
            (json!({ "version": 1, "commands": forward }), tp)
        };
        let resolved_base = match base_seq {
            Some(n) => n,
            None => {
                let sc_path = state_path
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| sidecar_path_for(ref_path, None));
                let sc_text = fs::read_to_string(&sc_path).map_err(|_| {
                    format!(
                        "--base-seq is required (no sync sidecar found at '{}')",
                        sc_path.display()
                    )
                })?;
                let sc: SyncSidecar = serde_json::from_str(&sc_text)
                    .map_err(|e| format!("invalid sync sidecar '{}': {e}", sc_path.display()))?;
                sc.head_seq
            }
        };
        let bytes = serde_json::to_vec(&batch_val).unwrap_or_default();
        let resolved_op = op_id
            .map(str::to_string)
            .unwrap_or_else(|| gen_id("cli", &bytes));
        json!({
            "v": 1,
            "type": "batch",
            "op_id": resolved_op,
            "base_seq": resolved_base,
            "batch": batch_val,
        })
    };

    let body_bytes = serde_json::to_vec(&payload)
        .map_err(|e| format!("failed to encode sync request: {e}"))?;
    let resp = http_client::request(
        "POST",
        &endpoint,
        Some(&token),
        Some(&body_bytes),
        Some("application/json"),
    )?;
    if resp.status != 200 {
        return Err(format_api_error("solo studio push", &resp));
    }
    let body = parse_json_response(&resp);
    if let Some(sc_path) = state_path
        && let Ok(sc_text) = fs::read_to_string(sc_path)
        && let Ok(mut sc) = serde_json::from_str::<SyncSidecar>(&sc_text)
        && let Some(new_head) = body.get("head_seq").and_then(Value::as_i64)
    {
        sc.head_seq = new_head;
        if let Ok(updated) = serde_json::to_string_pretty(&sc) {
            let _ = write_atomic_file(sc_path, &(updated + "\n"));
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
    } else {
        let kind = body.get("type").and_then(Value::as_str).unwrap_or("ok");
        match kind {
            "ack" => println!(
                "solo studio push: ack seq={} head_seq={} rebased={} duplicate={}",
                body.get("seq").and_then(Value::as_i64).unwrap_or(0),
                body.get("head_seq").and_then(Value::as_i64).unwrap_or(0),
                body.get("rebased").and_then(Value::as_bool).unwrap_or(false),
                body.get("duplicate").and_then(Value::as_bool).unwrap_or(false),
            ),
            "checkpoint" => println!(
                "solo studio push: checkpoint seq={} revision_id={} status={}",
                body.get("seq").and_then(Value::as_i64).unwrap_or(0),
                body.get("revision_id").and_then(Value::as_str).unwrap_or(""),
                body.get("status").and_then(Value::as_str).unwrap_or(""),
            ),
            "state" => println!(
                "solo studio push: seeded head_seq={}",
                body.get("head_seq").and_then(Value::as_i64).unwrap_or(0),
            ),
            _ => println!("solo studio push: {kind}"),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_publish(
    site_ref: &str,
    input: Option<&Path>,
    revision_id: Option<&str>,
    revision_sha256: Option<&str>,
    request_id: Option<&str>,
    upload_bundle: bool,
    wait: bool,
    activate: bool,
    url_override: Option<&str>,
    json_out: bool,
) -> Result<(), String> {
    if revision_id.is_some() != revision_sha256.is_some() {
        return Err("specify both --revision-id and --revision-sha256, or neither".to_string());
    }
    let (ws, site) = parse_site_ref(site_ref)?;
    let (base_url, token) = resolve_auth(url_override)?;

    // Stage 1: Obtain IR (local --input or pull from Studio) and run local shared Rust verification + compile.
    let raw_input = match input {
        Some(path) => fs::read_to_string(path)
            .map_err(|e| format!("failed to read '{}': {e}", path.display()))?,
        None => {
            let sync_url = format!("{base_url}/api/sites/{ws}/{site}/sync");
            let resp = http_client::request("GET", &sync_url, Some(&token), None, None)?;
            if resp.status != 200 {
                return Err(format_api_error("solo studio publish (pull)", &resp));
            }
            let body = parse_json_response(&resp);
            body.get("document")
                .and_then(Value::as_str)
                .ok_or_else(|| "Studio sync response missing 'document'".to_string())?
                .to_string()
        }
    };

    let raw_val: Value = serde_json::from_str(&raw_input)
        .map_err(|e| format!("invalid JSON input for publish: {e}"))?;
    let registry = default_registry();
    let check_report = run_checks_on_value(&raw_val, "<studio:publish>", None, &registry);
    if !check_report.ok {
        let msgs: Vec<String> = check_report
            .errors
            .iter()
            .map(|e| format!("{} ({}): {}", e.code, e.path, e.message))
            .collect();
        return Err(format!(
            "local verification failed ({} error(s)); refusing to publish: {}",
            check_report.summary.error_count,
            msgs.join("; ")
        ));
    }

    let mut compile_opts = CompileOptions {
        include_headers_file: false,
        ..CompileOptions::default()
    };
    let compiled = compile_from_json(&raw_input, &registry, &compile_opts)
        .map_err(|e| format!("local deterministic compile failed: {e}"))?;
    let compile_errors: Vec<String> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.level == DiagnosticLevel::Error)
        .map(|d| format!("{} ({}): {}", d.code, d.path, d.message))
        .collect();
    if !compile_errors.is_empty() {
        return Err(format!(
            "compiler reported error(s); refusing to publish: {}",
            compile_errors.join("; ")
        ));
    }
    let _ = &mut compile_opts;

    let reserved = ["_headers", "_redirects", "_routes.json", "_worker.js"];
    let mut manifest_files = Vec::new();
    let mut bundle_files = BTreeMap::new();
    let mut total_bytes: u64 = 0;
    for (path, content) in &compiled.files {
        if reserved.iter().any(|r| path == r || path.starts_with("_worker.js/")) {
            continue;
        }
        let bytes = content.as_bytes();
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        total_bytes = total_bytes.saturating_add(size);
        manifest_files.push(json!({
            "path": path,
            "sha256": sha256_hex(bytes),
            "size": size,
        }));
        bundle_files.insert(path.clone(), content.clone());
    }
    let file_count = u64::try_from(manifest_files.len()).unwrap_or(0);

    // Stage 2: Resolve revision_id + revision_sha256 (checkpoint if not pinned).
    let (rev_id, rev_sha) = match (revision_id, revision_sha256) {
        (Some(id), Some(sha)) => (id.to_string(), sha.to_string()),
        _ => {
            let sync_url = format!("{base_url}/api/sites/{ws}/{site}/sync");
            let cp_req = serde_json::to_vec(&json!({
                "v": 1,
                "type": "checkpoint",
                "reason": "publish",
            }))
            .unwrap_or_default();
            let cp_resp = http_client::request(
                "POST",
                &sync_url,
                Some(&token),
                Some(&cp_req),
                Some("application/json"),
            )?;
            if cp_resp.status != 200 {
                return Err(format_api_error("solo studio publish (checkpoint)", &cp_resp));
            }
            let cp_body = parse_json_response(&cp_resp);
            let id = cp_body
                .get("revision_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint response missing 'revision_id'".to_string())?
                .to_string();
            let sha = cp_body
                .get("ir_sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint response missing 'ir_sha256'".to_string())?
                .to_string();
            (id, sha)
        }
    };

    let manifest = json!({
        "format": "sws-artifact-manifest/1",
        "revision_id": rev_id,
        "revision_sha256": rev_sha,
        "compiler": {
            "name": "sws-cli",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "files": manifest_files,
        "file_count": file_count,
        "total_bytes": total_bytes,
    });
    let req_id = request_id
        .map(str::to_string)
        .unwrap_or_else(|| gen_id("c", rev_sha.as_bytes()));
    let enqueue_payload = serde_json::to_vec(&json!({
        "request_id": req_id,
        "manifest": manifest,
    }))
    .map_err(|e| format!("failed to encode publish request: {e}"))?;

    let publish_url = format!("{base_url}/api/sites/{ws}/{site}/publish");
    let pub_resp = http_client::request(
        "POST",
        &publish_url,
        Some(&token),
        Some(&enqueue_payload),
        Some("application/json"),
    )?;
    if pub_resp.status != 200 && pub_resp.status != 202 {
        return Err(format_api_error("solo studio publish (enqueue)", &pub_resp));
    }
    let mut job_body = parse_json_response(&pub_resp);
    let job_id = job_body
        .get("job_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let manifest_sha = job_body
        .pointer("/artifact/manifest_sha256")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut bundle_status = "skipped";
    if upload_bundle && !manifest_sha.is_empty() {
        let bundle_url = format!("{base_url}/api/sites/{ws}/{site}/publish/bundles/{manifest_sha}");
        let worker_bytes = b"export default { fetch() { return new Response('ok'); } };";
        let file_headers: Vec<Value> = bundle_files
            .iter()
            .map(|(path, content)| {
                json!({
                    "path": path,
                    "size": u64::try_from(content.len()).unwrap_or(u64::MAX),
                })
            })
            .collect();
        let header_json = serde_json::to_vec(&json!({
            "revision_id": rev_id,
            "revision_sha256": rev_sha,
            "compiler": {
                "name": "sws-cli",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "schema_level": 0,
            "worker": {
                "main_module": "index.js",
                "modules": [{
                    "name": "index.js",
                    "content_type": "application/javascript+module",
                    "size": worker_bytes.len() as u64,
                }],
            },
            "files": file_headers,
        }))
        .map_err(|e| format!("failed to encode bundle header: {e}"))?;
        let header_len = u32::try_from(header_json.len())
            .map_err(|_| "bundle header exceeds u32 length".to_string())?;
        let mut bundle_bytes = Vec::with_capacity(
            12 + header_json.len() + worker_bytes.len() + total_bytes as usize,
        );
        bundle_bytes.extend_from_slice(b"SWSBNDL1");
        bundle_bytes.extend_from_slice(&header_len.to_be_bytes());
        bundle_bytes.extend_from_slice(&header_json);
        bundle_bytes.extend_from_slice(worker_bytes);
        for content in bundle_files.values() {
            bundle_bytes.extend_from_slice(content.as_bytes());
        }
        let b_resp = http_client::request(
            "PUT",
            &bundle_url,
            Some(&token),
            Some(&bundle_bytes),
            Some("application/octet-stream"),
        )?;
        bundle_status = if (200..300).contains(&b_resp.status) {
            "uploaded"
        } else if b_resp.status == 404 || b_resp.status == 405 {
            "unavailable"
        } else {
            return Err(format_api_error("solo studio publish (bundle upload)", &b_resp));
        };
    }

    if wait && !job_id.is_empty() {
        let status_url = format!("{base_url}/api/sites/{ws}/{site}/publish/{job_id}");
        for _ in 0..60 {
            let phase = job_body.get("phase").and_then(Value::as_str).unwrap_or("");
            if phase == "ready" || phase == "failed" {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
            let s_resp = http_client::request("GET", &status_url, Some(&token), None, None)?;
            if s_resp.status != 200 {
                return Err(format_api_error("solo studio publish (wait)", &s_resp));
            }
            job_body = parse_json_response(&s_resp);
        }
    }

    let mut activation_status = "pending";
    if activate && !job_id.is_empty() {
        let act_url = format!("{base_url}/api/sites/{ws}/{site}/publish/{job_id}/activate");
        let a_resp = http_client::request("POST", &act_url, Some(&token), None, None)?;
        activation_status = if (200..300).contains(&a_resp.status) {
            "activated"
        } else if a_resp.status == 404 || a_resp.status == 405 {
            "unavailable"
        } else {
            return Err(format_api_error("solo studio publish (activate)", &a_resp));
        };
    }

    let phase = job_body.get("phase").and_then(Value::as_str).unwrap_or("queued");
    let origin = job_body.get("origin").and_then(Value::as_str).unwrap_or("cli");
    let server_verified = job_body
        .get("server_verified")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "origin": origin,
                "server_verified": server_verified,
                "stages": {
                    "compile": { "status": "ok", "origin": "cli", "file_count": file_count, "total_bytes": total_bytes },
                    "upload": { "status": "enqueued", "bundle": bundle_status, "manifest_sha256": manifest_sha },
                    "activation": { "status": activation_status, "job_phase": phase },
                },
                "job": job_body,
            }))
            .unwrap_or_default()
        );
    } else {
        println!(
            "[compile]    origin={origin} server_verified={server_verified} files={file_count} total_bytes={total_bytes} (local Rust verification + deterministic compile)"
        );
        println!(
            "[upload]     job_id={job_id} revision_id={rev_id} manifest_sha256={manifest_sha} bundle={bundle_status}"
        );
        println!(
            "[activation] phase={phase} activation={activation_status} origin={origin} (not server-verified)"
        );
    }
    Ok(())
}

fn cmd_status(
    site_ref: &str,
    job: &str,
    url_override: Option<&str>,
    json_out: bool,
) -> Result<(), String> {
    let (ws, site) = parse_site_ref(site_ref)?;
    if !is_valid_id(job) {
        return Err(format!("invalid job id '{job}'"));
    }
    let (base_url, token) = resolve_auth(url_override)?;
    let status_url = format!("{base_url}/api/sites/{ws}/{site}/publish/{job}");
    let resp = http_client::request("GET", &status_url, Some(&token), None, None)?;
    if resp.status != 200 {
        return Err(format_api_error("solo studio status", &resp));
    }
    let body = parse_json_response(&resp);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
    } else {
        let phase = body.get("phase").and_then(Value::as_str).unwrap_or("");
        let origin = body.get("origin").and_then(Value::as_str).unwrap_or("");
        let verified = body
            .get("server_verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let rev = body.get("revision_id").and_then(Value::as_str).unwrap_or("");
        let origin_note = if origin == "cli" {
            "cli (locally compiled; not server-verified)"
        } else {
            "server"
        };
        println!(
            "job_id={job} phase={phase} origin={origin_note} server_verified={verified} revision_id={rev}"
        );
    }
    Ok(())
}

fn cmd_enable_r2() {
    println!("Opt-in R2 media storage for sws-worker-studio (no resources provisioned):\n");
    println!("1. Cardless default vs. R2 quotas:");
    println!("   - Cardless default (D1 + Cron Trigger, R2 unbound):");
    println!("     * Workers Free: 100k requests/day, 10 ms CPU/req, Cron Trigger included");
    println!("     * D1 Free: 5M rows read/day, 100k rows written/day, 5 GB storage");
    println!("     * Durable Objects (SQLite): 100k req/day, 13k GB-s/day, 5 GB storage");
    println!("     * Media blobs: bounded D1/inline limits only; no R2 billing required");
    println!("   - Opt-in R2 (`MEDIA_BUCKET` + `STUDIO_ASSET_BACKEND=\"r2\"`):");
    println!("     * R2 Free tier: 10 GB-month storage, 1M Class A ops/month, 10M Class B ops/month, $0 egress");
    println!("     * Requires Cloudflare R2 enabled on the account.\n");
    println!("2. Exact wrangler commands to provision and bind R2:");
    println!("   npx wrangler r2 bucket create sws-studio-media");
    println!("   npx wrangler deploy -c wrangler.studio.jsonc\n");
    println!("3. Diff for wrangler.studio.jsonc:");
    println!("   + \"r2_buckets\": [");
    println!("   +   {{ \"binding\": \"MEDIA_BUCKET\", \"bucket_name\": \"sws-studio-media\" }}");
    println!("   + ],");
    println!("     \"vars\": {{");
    println!("   +   \"STUDIO_ASSET_BACKEND\": \"r2\",");
    println!("       ...");
    println!("     }}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_site_ref_validates_workspace_and_site() {
        assert_eq!(
            parse_site_ref("ws-a/site_1").unwrap(),
            ("ws-a".to_string(), "site_1".to_string())
        );
        assert!(parse_site_ref("ws-a").is_err());
        assert!(parse_site_ref("WS/site_1").is_err());
        assert!(parse_site_ref("ws-a/site/extra").is_err());
    }
}
