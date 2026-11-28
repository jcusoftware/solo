//! Stdio JSON-RPC 2.0 Model Context Protocol (MCP) server for `solo`.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sws_compiler::{DiagnosticLevel, compile_from_json};
use sws_ir::edit::{self, Command, CommandBatch, ContainerRef, EditOptions, next_id};
use sws_ir::{IrPatch, Node, SiteDocument};

use crate::commands::{
    compile_options_with_pack_extras, default_registry, parse_value_to_ir,
    parse_value_to_ir_unmerged, run_checks_on_value,
};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_READ_SITE_IR_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MCP_LINE_BYTES: usize = 4 * 1024 * 1024;

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
    mut reader: R,
    mut writer: W,
    workspace_root: &Path,
) -> io::Result<()> {
    let mut line_buf = Vec::new();
    loop {
        line_buf.clear();
        let mut oversized = false;
        loop {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                break;
            }
            if let Some(nl_pos) = chunk.iter().position(|&b| b == b'\n') {
                let take = nl_pos + 1;
                if !oversized {
                    if line_buf.len().saturating_add(take) > MAX_MCP_LINE_BYTES {
                        oversized = true;
                        line_buf.clear();
                    } else {
                        line_buf.extend_from_slice(&chunk[..take]);
                    }
                }
                reader.consume(take);
                break;
            }
            let chunk_len = chunk.len();
            if !oversized {
                if line_buf.len().saturating_add(chunk_len) > MAX_MCP_LINE_BYTES {
                    oversized = true;
                    line_buf.clear();
                } else {
                    line_buf.extend_from_slice(chunk);
                }
            }
            reader.consume(chunk_len);
        }

        if oversized {
            let err = json_rpc_error(
                Value::Null,
                -32600,
                "Invalid Request: message exceeds maximum size",
            );
            let serialized = serde_json::to_string(&err).unwrap_or_else(|e| {
                eprintln!("solo mcp: failed to serialize error: {e}");
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal serialization error"}}"#.to_string()
            });
            writeln!(writer, "{serialized}")?;
            writer.flush()?;
            continue;
        }

        if line_buf.is_empty() {
            break;
        }

        let line = match String::from_utf8(std::mem::take(&mut line_buf)) {
            Ok(s) => s,
            Err(_) => {
                let err = json_rpc_error(Value::Null, -32700, "Parse error: invalid UTF-8");
                writeln!(writer, "{err}")?;
                writer.flush()?;
                continue;
            }
        };
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
        "apply_command_batch" => tool_apply_command_batch(&args),
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
            name: "apply_command_batch",
            description: "Apply an atomic versioned CommandBatch to an upgraded SiteDocument IR via sws_ir::edit (uid addressing, lossless gate, never bakes pack contributions).",
            input_schema: json!({
                "type": "object",
                "required": ["ir_json"],
                "properties": {
                    "ir_json": { "description": "Upgraded SiteDocument JSON string or object." },
                    "batch_json": { "description": "CommandBatch JSON string or object ({ \"version\": 1, \"commands\": [...] })." },
                    "batch": { "description": "CommandBatch object ({ \"version\": 1, \"commands\": [...] })." },
                    "allow_lossy": { "type": "boolean", "description": "Proceed even if unsupported fields would be dropped (default false)." }
                }
            }),
        },
        ToolDef {
            name: "apply_ir_patch",
            description: "[DEPRECATED: use apply_command_batch] Apply one or more legacy IrPatch operations routed through the shared sws_ir::edit reducer without baking pack contributions.",
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

pub(crate) fn is_sensitive_component(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "private"
        || lower == ".git"
        || lower == ".wrangler"
        || lower == ".dev.vars"
        || lower == ".env"
        || lower == ".npmrc"
        || lower == ".netrc"
        || lower == "id_rsa"
        || lower == "id_ed25519"
        || lower == "id_ecdsa"
        || lower.starts_with(".env.")
        || lower.ends_with(".env")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with(".secret")
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
            return Err("access denied: path resolves outside workspace root".to_string());
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
        // Check workspace prefix before canonicalizing absolute paths so existing vs missing
        // files outside `workspace_root` cannot be distinguished via error messages.
        let abs_ok = req_path.starts_with(&canon_root) || req_path.starts_with(workspace_root);
        if !abs_ok {
            return Err("access denied: path resolves outside workspace root".to_string());
        }
        req_path.to_path_buf()
    } else {
        canon_root.join(req_path)
    };
    let canon_target = fs::canonicalize(&candidate)
        .map_err(|e| format!("failed to resolve path '{raw_path}': {e}"))?;

    if !canon_target.starts_with(&canon_root) {
        return Err("access denied: path resolves outside workspace root".to_string());
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

fn tool_apply_command_batch(args: &Value) -> Result<String, String> {
    let ir_val = args
        .get("ir_json")
        .ok_or_else(|| "apply_command_batch requires 'ir_json'".to_string())?;
    let ir_str = match ir_val {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other)
            .map_err(|e| format!("failed to serialize 'ir_json': {e}"))?,
    };

    // Refuse pack manifests so pack contributions are never baked into saved IR.
    let raw_val: Value =
        serde_json::from_str(&ir_str).map_err(|e| format!("invalid JSON in 'ir_json': {e}"))?;
    let registry = default_registry();
    if registry
        .list()
        .iter()
        .any(|plugin| plugin.can_transform_input(&raw_val))
    {
        return Err(
            "apply_command_batch requires an ejected SiteDocument IR (run `solo edit eject` first so pack contributions are not baked into saved IR)"
                .to_string(),
        );
    }

    let batch_val = args
        .get("batch_json")
        .or_else(|| args.get("batch"))
        .ok_or_else(|| "apply_command_batch requires 'batch_json' or 'batch'".to_string())?;
    let batch_str = match batch_val {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other)
            .map_err(|e| format!("failed to serialize 'batch': {e}"))?,
    };

    let allow_lossy = args
        .get("allow_lossy")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let opts = EditOptions { allow_lossy };
    let out = edit::apply_batch_json(&ir_str, &batch_str, opts).map_err(|e| e.to_string())?;

    // Prove the transaction is restorable via the shared reducer's undo path.
    let tx_json = serde_json::to_string(&out.transaction)
        .map_err(|e| format!("failed to serialize transaction: {e}"))?;
    edit::apply_undo_json(&out.document, &tx_json, opts)
        .map_err(|e| format!("verify_restorable failed: {e}"))?;

    let doc_val: Value = serde_json::from_str(&out.document)
        .map_err(|e| format!("failed to parse updated SiteDocument: {e}"))?;
    serde_json::to_string_pretty(&json!({
        "document": doc_val,
        "transaction": out.transaction,
        "dropped_paths": out.dropped_paths,
    }))
    .map_err(|e| format!("failed to serialize apply_command_batch output: {e}"))
}

fn find_node_by_id<'a>(nodes: &'a [Node], node_id: &str) -> Option<&'a Node> {
    for node in nodes {
        match node {
            Node::Element { id, children, .. } => {
                if id.as_deref() == Some(node_id) {
                    return Some(node);
                }
                if let Some(found) = find_node_by_id(children, node_id) {
                    return Some(found);
                }
            }
            Node::ComponentInstance { slots, .. } => {
                for slot_nodes in slots.values() {
                    if let Some(found) = find_node_by_id(slot_nodes, node_id) {
                        return Some(found);
                    }
                }
            }
            Node::Conditional {
                then_nodes,
                else_nodes,
                ..
            } => {
                if let Some(found) = find_node_by_id(then_nodes, node_id) {
                    return Some(found);
                }
                if let Some(found) = find_node_by_id(else_nodes, node_id) {
                    return Some(found);
                }
            }
            Node::Loop { body, .. } => {
                if let Some(found) = find_node_by_id(body, node_id) {
                    return Some(found);
                }
            }
            Node::SlotRef { fallback, .. } => {
                if let Some(found) = find_node_by_id(fallback, node_id) {
                    return Some(found);
                }
            }
            Node::Text { .. } => {}
        }
    }
    None
}

fn ensure_node_uids(doc: &SiteDocument, node: &mut Node) {
    if node.uid().is_none() {
        node.set_uid(Some(next_id(doc, "node")));
    }
    match node {
        Node::Element { children, .. } => {
            for c in children {
                ensure_node_uids(doc, c);
            }
        }
        Node::ComponentInstance { slots, .. } => {
            for list in slots.values_mut() {
                for c in list {
                    ensure_node_uids(doc, c);
                }
            }
        }
        Node::Conditional {
            then_nodes,
            else_nodes,
            ..
        } => {
            for c in then_nodes {
                ensure_node_uids(doc, c);
            }
            for c in else_nodes {
                ensure_node_uids(doc, c);
            }
        }
        Node::Loop { body, .. } => {
            for c in body {
                ensure_node_uids(doc, c);
            }
        }
        Node::SlotRef { fallback, .. } => {
            for c in fallback {
                ensure_node_uids(doc, c);
            }
        }
        Node::Text { .. } => {}
    }
}

fn ir_patch_to_commands(doc: &SiteDocument, patch: IrPatch) -> Result<Vec<Command>, String> {
    match patch {
        IrPatch::SetSiteMeta {
            name,
            locale,
            base_url,
        } => Ok(vec![Command::SetSiteMeta {
            name,
            locale,
            base_url,
        }]),
        IrPatch::SetColorToken { key, hex } => Ok(vec![Command::SetColorToken { key, hex }]),
        IrPatch::SetFontToken { slot, stack } => Ok(vec![Command::SetFontToken { slot, stack }]),
        IrPatch::UpsertPage { mut page } => {
            for node in &mut page.body {
                ensure_node_uids(doc, node);
            }
            Ok(vec![Command::UpsertPage { page }])
        }
        IrPatch::RemovePage { id } => Ok(vec![Command::RemovePage { id }]),
        IrPatch::UpsertStyleRule { rule } => Ok(vec![Command::UpsertStyleRule { rule }]),
        IrPatch::SetDataValue { key, value } => Ok(vec![Command::SetDataValue { key, value }]),
        IrPatch::SetNodeText {
            page_id,
            node_id,
            text,
        } => {
            let page = doc
                .pages
                .iter()
                .find(|p| p.id == page_id)
                .ok_or_else(|| format!("page '{page_id}' not found"))?;
            let node = find_node_by_id(&page.body, &node_id)
                .ok_or_else(|| format!("node '{node_id}' not found on page '{page_id}'"))?;
            match node {
                Node::Text { uid, .. } => {
                    let uid = uid
                        .clone()
                        .ok_or_else(|| format!("node '{node_id}' missing uid"))?;
                    Ok(vec![Command::SetText { uid, value: text }])
                }
                Node::Element { uid, children, .. } => {
                    let elem_uid = uid
                        .clone()
                        .ok_or_else(|| format!("node '{node_id}' missing uid"))?;
                    if children.len() == 1
                        && let Node::Text { uid: Some(cuid), .. } = &children[0]
                    {
                        return Ok(vec![Command::SetText {
                            uid: cuid.clone(),
                            value: text,
                        }]);
                    }
                    let mut cmds = Vec::new();
                    for child in children {
                        if let Some(cuid) = child.uid() {
                            cmds.push(Command::RemoveNode {
                                uid: cuid.to_string(),
                            });
                        }
                    }
                    cmds.push(Command::InsertNode {
                        container: ContainerRef::Node {
                            uid: elem_uid,
                            slot: None,
                        },
                        index: Some(0),
                        node: Node::Text {
                            uid: Some(next_id(doc, "text")),
                            value: text,
                        },
                    });
                    Ok(cmds)
                }
                _ => Err(format!("node '{node_id}' does not support text updates")),
            }
        }
        IrPatch::SetNodeAttr {
            page_id,
            node_id,
            attr,
            value,
        } => {
            let page = doc
                .pages
                .iter()
                .find(|p| p.id == page_id)
                .ok_or_else(|| format!("page '{page_id}' not found"))?;
            let node = find_node_by_id(&page.body, &node_id)
                .ok_or_else(|| format!("node '{node_id}' not found on page '{page_id}'"))?;
            let uid = node
                .uid()
                .map(str::to_string)
                .ok_or_else(|| format!("node '{node_id}' missing uid"))?;
            Ok(vec![Command::SetAttr { uid, attr, value }])
        }
        IrPatch::InsertChildNode {
            page_id,
            parent_id,
            index,
            mut node,
        } => {
            ensure_node_uids(doc, &mut node);
            let container = match parent_id {
                None => ContainerRef::PageBody { page_id },
                Some(pid) => {
                    let page = doc
                        .pages
                        .iter()
                        .find(|p| p.id == page_id)
                        .ok_or_else(|| format!("page '{page_id}' not found"))?;
                    let parent = find_node_by_id(&page.body, &pid)
                        .ok_or_else(|| format!("parent node '{pid}' not found on page '{page_id}'"))?;
                    let uid = parent
                        .uid()
                        .map(str::to_string)
                        .ok_or_else(|| format!("parent node '{pid}' missing uid"))?;
                    ContainerRef::Node { uid, slot: None }
                }
            };
            Ok(vec![Command::InsertNode {
                container,
                index,
                node,
            }])
        }
        IrPatch::RemoveNode { page_id, node_id } => {
            let page = doc
                .pages
                .iter()
                .find(|p| p.id == page_id)
                .ok_or_else(|| format!("page '{page_id}' not found"))?;
            let node = find_node_by_id(&page.body, &node_id)
                .ok_or_else(|| format!("node '{node_id}' not found on page '{page_id}'"))?;
            let uid = node
                .uid()
                .map(str::to_string)
                .ok_or_else(|| format!("node '{node_id}' missing uid"))?;
            Ok(vec![Command::RemoveNode { uid }])
        }
    }
}

fn tool_apply_ir_patch(args: &Value) -> Result<String, String> {
    let ir_val = args
        .get("ir_json")
        .ok_or_else(|| "apply_ir_patch requires 'ir_json'".to_string())?;
    let raw_ir = parse_json_arg(ir_val, "ir_json")?;
    let registry = default_registry();
    // Never bake pack contributions into saved IR.
    let (mut doc, _kind) = parse_value_to_ir_unmerged(&raw_ir, &registry)?;

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

    if doc.metadata.contains_key(edit::MARKER_KEY) {
        for (idx, patch) in patches.into_iter().enumerate() {
            let cmds = ir_patch_to_commands(&doc, patch)
                .map_err(|e| format!("patch[{idx}] failed: {e}"))?;
            let batch = CommandBatch::new(cmds);
            let (next_doc, _tx) = edit::apply_batch(&doc, &batch)
                .map_err(|e| format!("patch[{idx}] failed: {e}"))?;
            doc = next_doc;
        }
    } else {
        for (idx, patch) in patches.into_iter().enumerate() {
            sws_ir::apply_patch(&mut doc, &patch)
                .map_err(|e| format!("patch[{idx}] failed: {e}"))?;
        }
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
    let options = compile_options_with_pack_extras(&input_str)?;
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
