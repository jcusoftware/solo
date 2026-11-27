//! Versioned JSON wire protocol for site sessions (host-testable).
//!
//! Every message is one JSON text frame `{ "v": 1, "type": "...", ... }`.
//! Client frames are parsed strictly (unknown fields, wrong types, missing
//! fields and unknown versions are rejected with a typed [`ProtocolError`];
//! nothing here panics on input). The command batch itself is kept as raw text
//! and parsed by `sws_ir::edit::parse_batch`, so its duplicate-key, precision
//! and unknown-field checks apply unchanged.
//!
//! Client -> server
//! * `ping`
//! * `sync { since_seq? }`        -> `replay` (small gap) or `state`
//! * `seed { document }`          first document (publisher or owner), only when empty
//! * `batch { op_id, base_seq, batch }`
//! * `undo { op_id, target_seq }` collaborative undo = a new inverse command
//! * `checkpoint { reason: "save" | "publish" }`
//! * `presence { state }` / `presence_get`
//!
//! Server -> client: `welcome`, `pong`, `state`, `replay`, `ack`, `entry`,
//! `presence`, `checkpoint`, `conflict`, `error`, `revoked`.

// `ProtocolError` carries the full typed wire error (code, message, optional op id); it only
// travels on the cold rejection path, so boxing it would add noise without a measurable win.
#![allow(clippy::result_large_err)]

use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use sws_ir::edit::{CommandBatch, CommandError, parse_batch};

use super::rebase::Conflict;
use super::store::{LogEntry, PresenceRow};
use super::{
    MAX_COMMAND_MESSAGE_BYTES, MAX_MESSAGE_BYTES, MAX_SESSION_BATCH_COMMANDS,
    PRESENCE_STATE_MAX_BYTES, PROTOCOL_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointReason {
    Auto,
    Save,
    Publish,
}

impl CheckpointReason {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckpointReason::Auto => "auto",
            CheckpointReason::Save => "save",
            CheckpointReason::Publish => "publish",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Ping,
    Sync {
        since_seq: Option<i64>,
    },
    PresenceGet,
    Presence {
        /// Compact JSON object text, already size-checked.
        state: String,
    },
    Seed {
        document: String,
    },
    Batch {
        op_id: String,
        base_seq: i64,
        batch: CommandBatch,
    },
    Undo {
        op_id: String,
        target_seq: i64,
    },
    Checkpoint {
        reason: CheckpointReason,
    },
}

/// A rejected frame. `code` is a stable machine-readable string.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolError {
    pub code: &'static str,
    pub message: String,
    pub command_error: Option<CommandError>,
    /// The offending operation id, when it could be read.
    pub op_id: Option<String>,
}

impl ProtocolError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            command_error: None,
            op_id: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    v: u32,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    op_id: Option<String>,
    #[serde(default)]
    base_seq: Option<i64>,
    #[serde(default)]
    target_seq: Option<i64>,
    #[serde(default)]
    since_seq: Option<i64>,
    #[serde(default)]
    document: Option<String>,
    #[serde(default)]
    batch: Option<Box<RawValue>>,
    #[serde(default)]
    state: Option<Value>,
    #[serde(default)]
    reason: Option<String>,
}

/// `op_id`: 8-64 characters of `[A-Za-z0-9._:-]` (a client-generated idempotency key).
pub fn is_valid_op_id(s: &str) -> bool {
    (8..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn forbid(env: &Envelope, allowed: &[&str]) -> Result<(), ProtocolError> {
    let present = [
        ("op_id", env.op_id.is_some()),
        ("base_seq", env.base_seq.is_some()),
        ("target_seq", env.target_seq.is_some()),
        ("since_seq", env.since_seq.is_some()),
        ("document", env.document.is_some()),
        ("batch", env.batch.is_some()),
        ("state", env.state.is_some()),
        ("reason", env.reason.is_some()),
    ];
    for (name, set) in present {
        if set && !allowed.contains(&name) {
            return Err(ProtocolError::new(
                "invalid_message",
                format!("field `{name}` is not valid for `{}`", env.kind),
            ));
        }
    }
    Ok(())
}

fn need<T>(v: Option<T>, name: &str, kind: &str) -> Result<T, ProtocolError> {
    v.ok_or_else(|| {
        ProtocolError::new(
            "invalid_message",
            format!("`{kind}` requires field `{name}`"),
        )
    })
}

fn need_op_id(env: &Envelope) -> Result<String, ProtocolError> {
    let id = need(env.op_id.clone(), "op_id", &env.kind)?;
    if is_valid_op_id(&id) {
        Ok(id)
    } else {
        Err(ProtocolError::new(
            "invalid_op_id",
            "op_id must be 8-64 characters of [A-Za-z0-9._:-]",
        ))
    }
}

fn need_seq(v: Option<i64>, name: &str, kind: &str) -> Result<i64, ProtocolError> {
    let n = need(v, name, kind)?;
    if n < 0 {
        return Err(ProtocolError::new(
            "invalid_message",
            format!("`{name}` must not be negative"),
        ));
    }
    Ok(n)
}

/// Validate a presence state: a JSON object, compact form within the cap.
pub fn validate_presence_state(state: &Value) -> Result<String, ProtocolError> {
    if !state.is_object() {
        return Err(ProtocolError::new(
            "invalid_message",
            "presence `state` must be a JSON object",
        ));
    }
    let text = serde_json::to_string(state)
        .map_err(|_| ProtocolError::new("invalid_message", "presence `state` is not encodable"))?;
    if text.len() > PRESENCE_STATE_MAX_BYTES {
        return Err(ProtocolError::new(
            "presence_too_large",
            format!("presence `state` exceeds {PRESENCE_STATE_MAX_BYTES} bytes"),
        ));
    }
    Ok(text)
}

/// Parse one inbound text frame. `text.len()` is checked first, so oversized
/// frames never reach the JSON parser.
pub fn parse_client(text: &str) -> Result<ClientMessage, ProtocolError> {
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::new(
            "message_too_large",
            format!("message exceeds {MAX_MESSAGE_BYTES} bytes"),
        ));
    }
    let env: Envelope = serde_json::from_str(text)
        .map_err(|e| ProtocolError::new("invalid_message", format!("malformed message: {e}")))?;
    if env.v != PROTOCOL_VERSION {
        return Err(ProtocolError::new(
            "unsupported_version",
            format!(
                "protocol version {} is not supported (expected {PROTOCOL_VERSION})",
                env.v
            ),
        ));
    }
    if env.kind != "seed" && text.len() > MAX_COMMAND_MESSAGE_BYTES {
        return Err(ProtocolError::new(
            "message_too_large",
            format!(
                "`{}` messages are limited to {MAX_COMMAND_MESSAGE_BYTES} bytes",
                env.kind
            ),
        ));
    }
    match env.kind.as_str() {
        "ping" => {
            forbid(&env, &[])?;
            Ok(ClientMessage::Ping)
        }
        "presence_get" => {
            forbid(&env, &[])?;
            Ok(ClientMessage::PresenceGet)
        }
        "sync" => {
            forbid(&env, &["since_seq"])?;
            let since_seq = match env.since_seq {
                Some(n) => Some(need_seq(Some(n), "since_seq", "sync")?),
                None => None,
            };
            Ok(ClientMessage::Sync { since_seq })
        }
        "presence" => {
            forbid(&env, &["state"])?;
            let state = need(env.state.as_ref(), "state", "presence")?;
            Ok(ClientMessage::Presence {
                state: validate_presence_state(state)?,
            })
        }
        "seed" => {
            forbid(&env, &["document"])?;
            Ok(ClientMessage::Seed {
                document: need(env.document.clone(), "document", "seed")?,
            })
        }
        "batch" => {
            forbid(&env, &["op_id", "base_seq", "batch"])?;
            let op_id = need_op_id(&env)?;
            let fail = |mut e: ProtocolError| {
                e.op_id = Some(op_id.clone());
                e
            };
            let base_seq = need_seq(env.base_seq, "base_seq", "batch").map_err(fail)?;
            let raw = need(env.batch.as_ref(), "batch", "batch").map_err(fail)?;
            let batch = parse_batch(raw.get()).map_err(|ce| {
                fail(ProtocolError {
                    code: "invalid_batch",
                    message: ce.to_string(),
                    command_error: Some(ce),
                    op_id: None,
                })
            })?;
            batch.validate().map_err(|ce| {
                fail(ProtocolError {
                    code: "invalid_batch",
                    message: ce.to_string(),
                    command_error: Some(ce),
                    op_id: None,
                })
            })?;
            if batch.commands.len() > MAX_SESSION_BATCH_COMMANDS {
                return Err(fail(ProtocolError::new(
                    "batch_too_large",
                    format!("a session batch is limited to {MAX_SESSION_BATCH_COMMANDS} commands"),
                )));
            }
            Ok(ClientMessage::Batch {
                op_id,
                base_seq,
                batch,
            })
        }
        "undo" => {
            forbid(&env, &["op_id", "target_seq"])?;
            let op_id = need_op_id(&env)?;
            let target_seq = need_seq(env.target_seq, "target_seq", "undo").map_err(|mut e| {
                e.op_id = Some(op_id.clone());
                e
            })?;
            if target_seq == 0 {
                return Err(ProtocolError {
                    op_id: Some(op_id),
                    ..ProtocolError::new("invalid_message", "`target_seq` must be at least 1")
                });
            }
            Ok(ClientMessage::Undo { op_id, target_seq })
        }
        "checkpoint" => {
            forbid(&env, &["reason"])?;
            let reason = match need(env.reason.as_deref(), "reason", "checkpoint")? {
                "save" => CheckpointReason::Save,
                "publish" => CheckpointReason::Publish,
                other => {
                    return Err(ProtocolError::new(
                        "invalid_message",
                        format!("checkpoint reason `{other}` must be `save` or `publish`"),
                    ));
                }
            };
            Ok(ClientMessage::Checkpoint { reason })
        }
        other => Err(ProtocolError::new(
            "unknown_type",
            format!(
                "unknown message type `{}`",
                other.chars().take(32).collect::<String>()
            ),
        )),
    }
}

// --- Server messages ---------------------------------------------------------

fn msg(kind: &str, mut body: Value) -> Value {
    if let Some(o) = body.as_object_mut() {
        o.insert("v".into(), json!(PROTOCOL_VERSION));
        o.insert("type".into(), json!(kind));
    }
    body
}

pub fn welcome(
    client_id: &str,
    user_id: i64,
    role: &str,
    head_seq: i64,
    seeded: bool,
    instance_id: &str,
) -> Value {
    msg(
        "welcome",
        json!({
            "client_id": client_id, "user_id": user_id, "role": role,
            "head_seq": head_seq, "seeded": seeded, "instance_id": instance_id,
        }),
    )
}

pub fn pong(head_seq: i64, instance_id: &str) -> Value {
    msg(
        "pong",
        json!({ "head_seq": head_seq, "instance_id": instance_id }),
    )
}

pub fn state(head_seq: i64, document: &str, checkpoint_seq: Option<i64>) -> Value {
    msg(
        "state",
        json!({ "head_seq": head_seq, "document": document, "checkpoint_seq": checkpoint_seq }),
    )
}

pub fn entry_json(e: &LogEntry) -> Value {
    let tx: Value = serde_json::from_str(&e.tx_json).unwrap_or(Value::Null);
    json!({
        "seq": e.seq, "op_id": e.op_id, "user_id": e.user_id, "client_id": e.client_id,
        "kind": e.kind.as_str(), "target_seq": e.target_seq, "base_seq": e.base_seq,
        "rebased": e.rebased, "ts_ms": e.ts_ms, "transaction": tx,
    })
}

pub fn replay(head_seq: i64, entries: &[LogEntry]) -> Value {
    msg(
        "replay",
        json!({ "head_seq": head_seq, "entries": entries.iter().map(entry_json).collect::<Vec<_>>() }),
    )
}

pub fn entry(e: &LogEntry) -> Value {
    let mut v = entry_json(e);
    if let Some(o) = v.as_object_mut() {
        o.insert("v".into(), json!(PROTOCOL_VERSION));
        o.insert("type".into(), json!("entry"));
    }
    v
}

pub fn ack(op_id: &str, seq: i64, duplicate: bool, rebased: bool, head_seq: i64) -> Value {
    msg(
        "ack",
        json!({ "op_id": op_id, "seq": seq, "duplicate": duplicate, "rebased": rebased, "head_seq": head_seq }),
    )
}

pub fn conflict(op_id: Option<&str>, c: &Conflict) -> Value {
    msg(
        "conflict",
        json!({
            "op_id": op_id, "kind": c.kind.as_str(), "message": c.message,
            "head_seq": c.head_seq, "seqs": c.seqs, "paths": c.paths,
        }),
    )
}

pub fn error(code: &str, message: &str, op_id: Option<&str>, ce: Option<&CommandError>) -> Value {
    msg(
        "error",
        json!({
            "code": code, "message": message, "op_id": op_id,
            "command_error": ce.and_then(|c| serde_json::to_value(c).ok()),
        }),
    )
}

pub fn presence(now_ms: i64, rows: &[PresenceRow]) -> Value {
    let users: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "client_id": r.client_id, "user_id": r.user_id,
                "state": serde_json::from_str::<Value>(&r.state).unwrap_or(Value::Null),
                "expires_in_ms": (r.expires_ms - now_ms).max(0),
            })
        })
        .collect();
    msg("presence", json!({ "users": users }))
}

pub fn checkpoint(seq: i64, revision_id: &str, status: &str, reason: &str) -> Value {
    msg(
        "checkpoint",
        json!({ "seq": seq, "revision_id": revision_id, "status": status, "reason": reason }),
    )
}

pub fn revoked(reason: &str) -> Value {
    msg("revoked", json!({ "reason": reason }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch_text(extra: &str) -> String {
        format!(
            r#"{{"v":1,"type":"batch","op_id":"op-00000001","base_seq":0{extra},"batch":{{"version":1,"commands":[{{"op":"set_text","uid":"a","value":"x"}}]}}}}"#
        )
    }

    #[test]
    fn parses_every_client_message() {
        assert_eq!(
            parse_client(r#"{"v":1,"type":"ping"}"#),
            Ok(ClientMessage::Ping)
        );
        assert_eq!(
            parse_client(r#"{"v":1,"type":"sync","since_seq":4}"#),
            Ok(ClientMessage::Sync { since_seq: Some(4) })
        );
        assert_eq!(
            parse_client(r#"{"v":1,"type":"sync"}"#),
            Ok(ClientMessage::Sync { since_seq: None })
        );
        assert_eq!(
            parse_client(r#"{"v":1,"type":"presence_get"}"#),
            Ok(ClientMessage::PresenceGet)
        );
        assert!(matches!(
            parse_client(r#"{"v":1,"type":"presence","state":{"cursor":"a"}}"#),
            Ok(ClientMessage::Presence { .. })
        ));
        assert!(matches!(
            parse_client(&batch_text("")),
            Ok(ClientMessage::Batch { base_seq: 0, .. })
        ));
        assert_eq!(
            parse_client(r#"{"v":1,"type":"undo","op_id":"op-00000002","target_seq":3}"#),
            Ok(ClientMessage::Undo {
                op_id: "op-00000002".into(),
                target_seq: 3
            })
        );
        assert_eq!(
            parse_client(r#"{"v":1,"type":"checkpoint","reason":"publish"}"#),
            Ok(ClientMessage::Checkpoint {
                reason: CheckpointReason::Publish
            })
        );
        assert!(matches!(
            parse_client(r#"{"v":1,"type":"seed","document":"{}"}"#),
            Ok(ClientMessage::Seed { .. })
        ));
    }

    #[test]
    fn rejects_bad_frames_without_panicking() {
        let code = |s: &str| parse_client(s).unwrap_err().code;
        assert_eq!(code(""), "invalid_message");
        assert_eq!(code("[]"), "invalid_message");
        assert_eq!(code(r#"{"v":1}"#), "invalid_message");
        assert_eq!(code(r#"{"v":2,"type":"ping"}"#), "unsupported_version");
        assert_eq!(code(r#"{"v":1,"type":"nope"}"#), "unknown_type");
        assert_eq!(
            code(r#"{"v":1,"type":"ping","extra":1}"#),
            "invalid_message"
        );
        assert_eq!(
            code(r#"{"v":1,"type":"ping","op_id":"op-00000001"}"#),
            "invalid_message"
        );
        assert_eq!(
            code(r#"{"v":1,"type":"sync","since_seq":-1}"#),
            "invalid_message"
        );
        assert_eq!(
            code(r#"{"v":1,"type":"sync","since_seq":"x"}"#),
            "invalid_message"
        );
        assert_eq!(
            code(r#"{"v":1,"type":"undo","op_id":"x","target_seq":1}"#),
            "invalid_op_id"
        );
        assert_eq!(
            code(r#"{"v":1,"type":"undo","op_id":"op-00000002","target_seq":0}"#),
            "invalid_message"
        );
        assert_eq!(
            code(r#"{"v":1,"type":"checkpoint","reason":"auto"}"#),
            "invalid_message"
        );
        assert_eq!(
            code(r#"{"v":1,"type":"presence","state":[1]}"#),
            "invalid_message"
        );
        assert_eq!(code(r#"{"v":1,"type":"seed"}"#), "invalid_message");
    }

    #[test]
    fn batch_errors_carry_the_op_id_and_command_error() {
        let bad = r#"{"v":1,"type":"batch","op_id":"op-00000001","base_seq":0,"batch":{"version":1,"commands":[{"op":"set_text","uid":"a","value":"x","bogus":1}]}}"#;
        let e = parse_client(bad).unwrap_err();
        assert_eq!(e.code, "invalid_batch");
        assert_eq!(e.op_id.as_deref(), Some("op-00000001"));
        assert!(e.command_error.is_some());
        let empty = r#"{"v":1,"type":"batch","op_id":"op-00000001","base_seq":0,"batch":{"version":1,"commands":[]}}"#;
        assert_eq!(parse_client(empty).unwrap_err().code, "invalid_batch");
        let ver = r#"{"v":1,"type":"batch","op_id":"op-00000001","base_seq":0,"batch":{"version":9,"commands":[{"op":"set_text","uid":"a","value":"x"}]}}"#;
        assert_eq!(parse_client(ver).unwrap_err().code, "invalid_batch");
    }

    #[test]
    fn batch_size_bounds() {
        let cmds =
            vec![r#"{"op":"set_text","uid":"a","value":"x"}"#; MAX_SESSION_BATCH_COMMANDS + 1];
        let big = format!(
            r#"{{"v":1,"type":"batch","op_id":"op-00000001","base_seq":0,"batch":{{"version":1,"commands":[{}]}}}}"#,
            cmds.join(",")
        );
        assert_eq!(parse_client(&big).unwrap_err().code, "batch_too_large");
        let huge = format!(
            r#"{{"v":1,"type":"presence","state":{{"k":"{}"}}}}"#,
            "x".repeat(MAX_COMMAND_MESSAGE_BYTES)
        );
        assert_eq!(parse_client(&huge).unwrap_err().code, "message_too_large");
        let over = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert_eq!(parse_client(&over).unwrap_err().code, "message_too_large");
    }

    #[test]
    fn seed_may_exceed_the_command_cap() {
        let doc = "x".repeat(MAX_COMMAND_MESSAGE_BYTES + 10);
        let text = format!(r#"{{"v":1,"type":"seed","document":"{doc}"}}"#);
        assert!(matches!(
            parse_client(&text),
            Ok(ClientMessage::Seed { .. })
        ));
    }

    #[test]
    fn presence_state_is_bounded() {
        let ok = format!(
            r#"{{"v":1,"type":"presence","state":{{"k":"{}"}}}}"#,
            "x".repeat(100)
        );
        assert!(parse_client(&ok).is_ok());
        let big = format!(
            r#"{{"v":1,"type":"presence","state":{{"k":"{}"}}}}"#,
            "x".repeat(PRESENCE_STATE_MAX_BYTES)
        );
        assert_eq!(parse_client(&big).unwrap_err().code, "presence_too_large");
    }

    #[test]
    fn op_id_grammar() {
        assert!(is_valid_op_id("op-0123456789.abc:_"));
        assert!(!is_valid_op_id("short"));
        assert!(!is_valid_op_id(&"a".repeat(65)));
        assert!(!is_valid_op_id("has space 12345"));
    }

    #[test]
    fn server_messages_are_versioned() {
        for m in [
            welcome("c", 1, "editor", 0, false, "i"),
            pong(0, "i"),
            state(1, "{}", None),
            replay(1, &[]),
            ack("op", 1, false, false, 1),
            presence(0, &[]),
            checkpoint(1, "r", "written", "save"),
            revoked("membership_revoked"),
            error("x", "y", None, None),
        ] {
            assert_eq!(m["v"], 1, "{m}");
            assert!(m["type"].is_string(), "{m}");
        }
    }
}
