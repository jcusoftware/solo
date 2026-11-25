//! Stdio JSON-RPC 2.0 Model Context Protocol (MCP) server for `solo`.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sws_compiler::{DiagnosticLevel, compile_from_json};
use sws_ir::{IrPatch, apply_patch};

use crate::commands::{
    default_compile_options, default_registry, parse_value_to_ir, run_checks_on_value,
};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_READ_SITE_IR_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
struct ToolDef {
    name: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreviewDiagnostic {
    level: String,
    code: String,
    path: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
struct CompilePreviewOutput {
    files: std::collections::BTreeMap<String, String>,
    index_html_bytes: usize,
    styles_css_bytes: usize,
    total_bytes: usize,
    diagnostics: Vec<PreviewDiagnostic>,
}

/// Run the stdio MCP server loop on standard input/output jailed to `workspace_root`.
pub fn run_stdio(workspace_root: &Path) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let reader = stdin.lock();
    let writer = stdout.lock();
    serve_mcp(reader, writer, workspace_root).map_err(|e| format!("MCP I/O error: {e}"))
}

/// Serve newline-delimited JSON-RPC 2.0 MCP messages from `reader` to `writer`
/// with filesystem reads confined to `workspace_root`.
pub fn serve_mcp<R: BufRead, W: Write>(
    reader: R,
    mut writer: W,
    workspace_root: &Path,
) -> io::Result<()> {
    for line_res in reader.lines() {
        let line = line_res?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(response) = handle_mcp_line(trimmed, workspace_root) {
            let serialized = serde_json::to_string(&response).unwrap_or_else(|e| {
                eprintln!("solo mcp: failed to serialize response: {e}");
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal serialization error"}}"#.to_string()
            });
            writeln!(writer, "{serialized}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Process a single JSON-RPC 2.0 line and return `Some(response)` unless the message is a notification.
#[must_use]
pub fn handle_mcp_line(line: &str, workspace_root: &Path) -> Option<Value> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(err) => {
            return Some(json_rpc_error(
                Value::Null,
                -32700,
                &format!("Parse error: {err}"),
            ));
        }
    };

    let Some(obj) = req.as_object() else {
        return Some(json_rpc_error(
            Value::Null,
            -32600,
            "Invalid Request: expected JSON object",
        ));
    };

    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return id.map(|req_id| {
            json_rpc_error(req_id, -32600, "Invalid Request: missing string 'method'")
        });
    };

    // Notifications (requests without an `id`) never receive a JSON-RPC response.
    let Some(req_id) = id else {
        if method != "notifications/initialized" && method != "notifications/cancelled" {
            eprintln!("solo mcp: ignoring unknown notification '{method}'");
        }
        return None;
    };

    let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));

    let response = match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(MCP_PROTOCOL_VERSION);
            let protocol_version = match requested {
                "2024-11-05" | "2025-03-26" => requested,
                _ => MCP_PROTOCOL_VERSION,
            };
            json_rpc_ok(
                req_id,
                json!({
                    "protocolVersion": protocol_version,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "solo-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
        }
        "ping" => json_rpc_ok(req_id, json!({})),
        "tools/list" => json_rpc_ok(req_id, json!({ "tools": tool_definitions() })),
        "tools/call" => handle_tools_call(req_id, &params, workspace_root),
        _ => json_rpc_error(req_id, -32601, &format!("Method not found: {method}")),
    };

    Some(response)
}

fn handle_tools_call(req_id: Value, params: &Value, workspace_root: &Path) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return json_rpc_error(req_id, -32602, "Invalid params: missing tool 'name'");
    };
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

    let result = match name {
        "read_site_ir" => tool_read_site_ir(&args, workspace_root),
        "apply_ir_patch" => tool_apply_ir_patch(&args),
        "compile_preview" => tool_compile_preview(&args),
        "run_a11y_and_plugin_checks" => tool_run_a11y_and_plugin_checks(&args),
        _ => {
            return json_rpc_error(req_id, -32602, &format!("Unknown tool: {name}"));
        }
    };

    match result {
        Ok(text) => json_rpc_ok(
            req_id,
            json!({
                "content": [{ "type": "text", "text": text }],
                "isError": false
            }),
        ),
        Err(err_msg) => json_rpc_ok(
            req_id,
            json!({
                "content": [{ "type": "text", "text": err_msg }],
                "isError": true
            }),
        ),
    }
}

fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read_site_ir",
            description: "Read a SiteDocument IR JSON or plugin manifest (such as venue.json) from a file path or JSON payload and return normalized SiteDocument JSON.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to a site.ir.json or venue.json file." },
                    "ir_json": { "description": "Inline SiteDocument or venue.json as a JSON string or object." }
                }
            }),
        },
        ToolDef {
            name: "apply_ir_patch",
            description: "Apply one or more declarative IrPatch operations to a SiteDocument and return the updated SiteDocument JSON.",
            input_schema: json!({
                "type": "object",
                "required": ["ir_json"],
                "properties": {
                    "ir_json": { "description": "SiteDocument JSON string or object." },
                    "patch_json": { "description": "Single IrPatch or array of IrPatch as JSON string or value." },
                    "patches": { "description": "Single IrPatch or array of IrPatch values." }
                }
            }),
        },
        ToolDef {
            name: "compile_preview",
            description: "Compile a SiteDocument IR or plugin manifest (such as venue.json) in-memory and return generated files, byte sizes, and diagnostics.",
            input_schema: json!({
                "type": "object",
                "required": ["ir_json"],
                "properties": {
                    "ir_json": { "description": "SiteDocument or venue.json as a JSON string or object." }
                }
            }),
        },
        ToolDef {
            name: "run_a11y_and_plugin_checks",
            description: "Run IR validation, WCAG/contrast compiler checks, plugin diagnostics, and advisory hospitality/ABC lint on a full or partial JSON document.",
            input_schema: json!({
                "type": "object",
                "required": ["input_json"],
                "properties": {
                    "input_json": { "description": "Full SiteDocument, venue.json, or partial JSON fixture as a JSON string or object." }
                }
            }),
        },
    ]
}

fn parse_json_arg(val: &Value, field_name: &str) -> Result<Value, String> {
    match val {
        Value::String(s) => serde_json::from_str(s)
            .map_err(|e| format!("invalid JSON in '{field_name}': {e}")),
        other => Ok(other.clone()),
    }
}

fn is_sensitive_component(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "private"
        || lower == ".git"
        || lower == ".dev.vars"
        || lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".env")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
}

fn check_no_sensitive_components(path: &Path) -> Result<(), String> {
    for comp in path.components() {
        if let Component::Normal(part) = comp {
            let name = part.to_string_lossy();
            if is_sensitive_component(&name) {
                return Err(format!(
                    "access denied: sensitive path component '{name}' is blocked"
                ));
            }
        }
    }
    Ok(())
}

fn resolve_safe_read_path(raw_path: &str, workspace_root: &Path) -> Result<PathBuf, String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return Err("path must not be empty or contain NUL bytes".to_string());
    }

    let req_path = Path::new(trimmed);
    for comp in req_path.components() {
        if matches!(comp, Component::ParentDir) {
            return Err(format!("access denied: '..' traversal not allowed in '{raw_path}'"));
        }
    }
    check_no_sensitive_components(req_path)?;

    let canon_root = fs::canonicalize(workspace_root).map_err(|e| {
        format!(
            "failed to resolve workspace root '{}': {e}",
            workspace_root.display()
        )
    })?;

    let candidate = if req_path.is_absolute() {
        req_path.to_path_buf()
    } else {
        canon_root.join(req_path)
    };
    let canon_target = fs::canonicalize(&candidate)
        .map_err(|e| format!("failed to resolve path '{raw_path}': {e}"))?;

    if !canon_target.starts_with(&canon_root) {
        return Err(format!(
            "access denied: path '{raw_path}' resolves outside workspace root"
        ));
    }

    let rel_target = canon_target
        .strip_prefix(&canon_root)
        .map_err(|_| "access denied: invalid relative path".to_string())?;
    check_no_sensitive_components(rel_target)?;

    let meta = fs::metadata(&canon_target)
        .map_err(|e| format!("failed to inspect '{raw_path}': {e}"))?;
    if !meta.is_file() {
        return Err(format!("path '{raw_path}' is not a regular file"));
    }
    if meta.len() > MAX_READ_SITE_IR_BYTES {
        return Err(format!(
            "file '{raw_path}' exceeds {} byte limit",
            MAX_READ_SITE_IR_BYTES
        ));
    }

    Ok(canon_target)
}

fn tool_read_site_ir(args: &Value, workspace_root: &Path) -> Result<String, String> {
    let raw = if let Some(ir_val) = args.get("ir_json") {
        parse_json_arg(ir_val, "ir_json")?
    } else if let Some(path_str) = args.get("path").and_then(Value::as_str) {
        let safe_path = resolve_safe_read_path(path_str, workspace_root)?;
        let content = fs::read_to_string(&safe_path)
            .map_err(|e| format!("failed to read '{path_str}': {e}"))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("invalid JSON in '{path_str}': {e}"))?
    } else {
        return Err("read_site_ir requires either 'path' or 'ir_json'".to_string());
    };

    let registry = default_registry();
    let (doc, _kind) = parse_value_to_ir(&raw, &registry)?;
    serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("failed to serialize SiteDocument: {e}"))
}

fn tool_apply_ir_patch(args: &Value) -> Result<String, String> {
    let ir_val = args
        .get("ir_json")
        .ok_or_else(|| "apply_ir_patch requires 'ir_json'".to_string())?;
    let raw_ir = parse_json_arg(ir_val, "ir_json")?;
    let registry = default_registry();
    let (mut doc, _kind) = parse_value_to_ir(&raw_ir, &registry)?;

    let patch_raw = if let Some(p) = args.get("patch_json") {
        parse_json_arg(p, "patch_json")?
    } else if let Some(p) = args.get("patches") {
        parse_json_arg(p, "patches")?
    } else {
        return Err("apply_ir_patch requires 'patch_json' or 'patches'".to_string());
    };

    let patches: Vec<IrPatch> = match patch_raw {
        Value::Array(_) => serde_json::from_value(patch_raw)
            .map_err(|e| format!("invalid patch array: {e}"))?,
        single => {
            let patch: IrPatch = serde_json::from_value(single)
                .map_err(|e| format!("invalid IrPatch object: {e}"))?;
            vec![patch]
        }
    };

    for (idx, patch) in patches.iter().enumerate() {
        apply_patch(&mut doc, patch)
            .map_err(|e| format!("patch[{idx}] failed: {e}"))?;
    }

    serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("failed to serialize updated SiteDocument: {e}"))
}

fn tool_compile_preview(args: &Value) -> Result<String, String> {
    let ir_val = args
        .get("ir_json")
        .ok_or_else(|| "compile_preview requires 'ir_json'".to_string())?;
    let input_str = match ir_val {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other)
            .map_err(|e| format!("failed to serialize 'ir_json': {e}"))?,
    };

    let registry = default_registry();
    let options = default_compile_options();
    let compiled = compile_from_json(&input_str, &registry, &options)
        .map_err(|e| format!("compilation failed: {e}"))?;

    let diagnostics = compiled
        .diagnostics
        .into_iter()
        .map(|d| PreviewDiagnostic {
            level: match d.level {
                DiagnosticLevel::Error => "error".to_string(),
                DiagnosticLevel::Warning => "warning".to_string(),
            },
            code: d.code,
            path: d.path,
            message: d.message,
        })
        .collect();

    let output = CompilePreviewOutput {
        total_bytes: compiled.index_html_bytes + compiled.styles_css_bytes,
        index_html_bytes: compiled.index_html_bytes,
        styles_css_bytes: compiled.styles_css_bytes,
        files: compiled.files,
        diagnostics,
    };

    serde_json::to_string_pretty(&output)
        .map_err(|e| format!("failed to serialize compile preview: {e}"))
}

fn tool_run_a11y_and_plugin_checks(args: &Value) -> Result<String, String> {
    let input_val = args
        .get("input_json")
        .ok_or_else(|| "run_a11y_and_plugin_checks requires 'input_json'".to_string())?;
    let raw = parse_json_arg(input_val, "input_json")?;
    let registry = default_registry();
    let report = run_checks_on_value(&raw, "<mcp:input_json>", None, &registry);
    serde_json::to_string_pretty(&report)
        .map_err(|e| format!("failed to serialize check report: {e}"))
}

fn json_rpc_ok(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}
