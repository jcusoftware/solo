//! Conflict rules for stale batches and collaborative undo (pure).
//!
//! A batch is **stale** when its `base_seq` is behind the log head. The rules
//! are deliberately narrow:
//!
//! * A stale batch is **rebased** (applied at the head) only when every command
//!   is an explicitly listed independent *field update* ([`is_rebaseable`]) and
//!   none of its touched fields overlaps a field touched by any entry after
//!   `base_seq` (`sws_ir::edit::touched_conflict`: equal paths or `/`-ancestors).
//! * Any other stale command (insert, remove, move, page and style-rule
//!   structure, token or data removal) is **rejected** as `stale_structural`,
//!   even when no field overlaps: order-dependent edits are re-sent against the
//!   current state by the client.
//! * Collaborative **undo** is a new log entry that applies the target's
//!   recorded inverse. It is refused as `undo_incompatible` when any later
//!   entry touched an overlapping field (including a later undo of the same
//!   entry), so one user's undo never silently reverts another user's work.
//!
//! Every conflict names the head, the conflicting sequence numbers and paths
//! and says what to do next, so a client can act on it without guessing.

use serde::Serialize;
use sws_ir::edit::{Command, touched_conflict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// Stale batch containing a structural (non field-update) command.
    StaleStructural,
    /// Stale batch whose fields overlap a concurrent change.
    FieldConflict,
    /// Stale batch whose targets no longer exist or no longer apply at the head.
    TargetChanged,
    /// `base_seq` is older than the retained log; the client must resync.
    BaseTooOld,
    /// Undo target was changed by a later entry.
    UndoIncompatible,
    /// Undo target is no longer in the retained log.
    UndoTooOld,
}

impl ConflictKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictKind::StaleStructural => "stale_structural",
            ConflictKind::FieldConflict => "field_conflict",
            ConflictKind::TargetChanged => "target_changed",
            ConflictKind::BaseTooOld => "base_too_old",
            ConflictKind::UndoIncompatible => "undo_incompatible",
            ConflictKind::UndoTooOld => "undo_too_old",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub kind: ConflictKind,
    /// Actionable explanation (what happened and what to do).
    pub message: String,
    pub head_seq: i64,
    /// Concurrent entries involved (capped).
    pub seqs: Vec<i64>,
    /// Overlapping touched paths (capped).
    pub paths: Vec<String>,
}

const LIST_CAP: usize = 20;

fn capped<T>(mut v: Vec<T>) -> Vec<T> {
    v.truncate(LIST_CAP);
    v
}

/// The explicit allowlist of independent field updates that may be rebased.
///
/// Each only overwrites one addressed field (`touched` is a single field path)
/// and is idempotent. `UpsertStyleRule` is excluded (it may append and so
/// depends on rule order), as are all inserts, removes and moves.
pub fn is_rebaseable(command: &Command) -> bool {
    matches!(
        command,
        Command::SetSiteMeta { .. }
            | Command::SetColorToken { .. }
            | Command::SetFontToken { .. }
            | Command::SetDataValue { .. }
            | Command::SetAttr { .. }
            | Command::SetText { .. }
            | Command::SetTag { .. }
            | Command::SetClasses { .. }
            | Command::SetStyleRefs { .. }
    )
}

/// Name of the first non-rebaseable command, if any (`(index, op name)`).
pub fn first_structural(commands: &[Command]) -> Option<(usize, String)> {
    commands.iter().enumerate().find_map(|(i, c)| {
        (!is_rebaseable(c)).then(|| {
            let op = serde_json::to_value(c)
                .ok()
                .and_then(|v| v.get("op").and_then(|o| o.as_str()).map(str::to_string))
                .unwrap_or_else(|| "command".into());
            (i, op)
        })
    })
}

/// A stale batch must consist of rebaseable field updates only.
pub fn check_rebaseable(
    commands: &[Command],
    base_seq: i64,
    head_seq: i64,
) -> Result<(), Conflict> {
    match first_structural(commands) {
        None => Ok(()),
        Some((index, op)) => Err(Conflict {
            kind: ConflictKind::StaleStructural,
            message: format!(
                "command {index} ({op}) is structural and the batch is based on seq {base_seq} \
                 but the document is at seq {head_seq}; only independent field updates are \
                 rebased. Apply the {} newer entries (send `sync` with since_seq {base_seq}) and \
                 resend this batch with base_seq {head_seq}.",
                head_seq - base_seq
            ),
            head_seq,
            seqs: capped(((base_seq + 1)..=head_seq).collect()),
            paths: Vec::new(),
        }),
    }
}

/// Concurrent entries (after `base_seq`) must not touch the batch's fields.
pub fn check_field_conflicts(
    touched: &[String],
    concurrent: &[(i64, Vec<String>)],
    base_seq: i64,
    head_seq: i64,
) -> Result<(), Conflict> {
    let mut seqs = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    for (seq, other) in concurrent {
        if touched_conflict(touched, other) {
            seqs.push(*seq);
            for mine in touched {
                if other.iter().any(|o| sws_ir::edit::paths_conflict(mine, o))
                    && !paths.contains(mine)
                {
                    paths.push(mine.clone());
                }
            }
        }
    }
    if seqs.is_empty() {
        return Ok(());
    }
    Err(Conflict {
        kind: ConflictKind::FieldConflict,
        message: format!(
            "the batch is based on seq {base_seq} but seq {} changed the same field(s) ({}). \
             Apply the newer entries (send `sync` with since_seq {base_seq}), re-evaluate your \
             edit, and resend it with base_seq {head_seq}.",
            seqs.iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            paths.join(", ")
        ),
        head_seq,
        seqs: capped(seqs),
        paths: capped(paths),
    })
}

/// Undo is allowed only if no later entry touched an overlapping field.
pub fn check_undo(
    target_seq: i64,
    target_touched: &[String],
    later: &[(i64, Vec<String>)],
    head_seq: i64,
) -> Result<(), Conflict> {
    let mut seqs = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    for (seq, other) in later {
        if touched_conflict(target_touched, other) {
            seqs.push(*seq);
            for mine in target_touched {
                if other.iter().any(|o| sws_ir::edit::paths_conflict(mine, o))
                    && !paths.contains(mine)
                {
                    paths.push(mine.clone());
                }
            }
        }
    }
    if seqs.is_empty() {
        return Ok(());
    }
    Err(Conflict {
        kind: ConflictKind::UndoIncompatible,
        message: format!(
            "cannot undo seq {target_seq}: seq {} later changed the same field(s) ({}). \
             Undo those changes first, or make the edit you want as a new batch.",
            seqs.iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            paths.join(", ")
        ),
        head_seq,
        seqs: capped(seqs),
        paths: capped(paths),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sws_ir::edit::ContainerRef;

    fn set_attr(uid: &str) -> Command {
        Command::SetAttr {
            uid: uid.into(),
            attr: "href".into(),
            value: Some("/x".into()),
        }
    }

    #[test]
    fn allowlist_is_explicit() {
        assert!(is_rebaseable(&set_attr("a")));
        assert!(is_rebaseable(&Command::SetColorToken {
            key: "bg".into(),
            hex: "#000000".into()
        }));
        assert!(!is_rebaseable(&Command::RemoveNode { uid: "a".into() }));
        assert!(!is_rebaseable(&Command::MoveNode {
            uid: "a".into(),
            container: ContainerRef::PageBody {
                page_id: "home".into()
            },
            index: None
        }));
        assert!(!is_rebaseable(&Command::RemoveColorToken {
            key: "bg".into()
        }));
        assert!(!is_rebaseable(&Command::MovePage {
            id: "home".into(),
            index: 0
        }));
    }

    #[test]
    fn structural_names_the_command() {
        let cmds = vec![set_attr("a"), Command::RemoveNode { uid: "b".into() }];
        let c = check_rebaseable(&cmds, 2, 5).unwrap_err();
        assert_eq!(c.kind, ConflictKind::StaleStructural);
        assert!(
            c.message.contains("command 1 (remove_node)"),
            "{}",
            c.message
        );
        assert_eq!(c.seqs, vec![3, 4, 5]);
        assert!(check_rebaseable(&[set_attr("a")], 2, 5).is_ok());
    }

    #[test]
    fn independent_fields_pass_overlaps_fail() {
        let mine = vec!["node:a/attrs/href".to_string()];
        let other_field = vec![(3, vec!["node:a/attrs/title".to_string()])];
        assert!(check_field_conflicts(&mine, &other_field, 2, 3).is_ok());
        let same = vec![(3, vec!["node:a/attrs/href".to_string()])];
        let c = check_field_conflicts(&mine, &same, 2, 3).unwrap_err();
        assert_eq!(
            (c.kind, c.seqs, c.paths),
            (
                ConflictKind::FieldConflict,
                vec![3],
                vec!["node:a/attrs/href".to_string()]
            )
        );
        // Ancestor overlap (the node was removed) also conflicts.
        let removed = vec![(3, vec!["node:a".to_string()])];
        assert!(check_field_conflicts(&mine, &removed, 2, 3).is_err());
    }

    #[test]
    fn undo_needs_a_clean_tail() {
        let target = vec!["node:a/text".to_string()];
        let later = vec![(8, vec!["node:b/text".to_string()])];
        assert!(check_undo(7, &target, &later, 8).is_ok());
        let later = vec![(8, vec!["node:a/text".to_string()])];
        let c = check_undo(7, &target, &later, 8).unwrap_err();
        assert_eq!(c.kind, ConflictKind::UndoIncompatible);
        assert_eq!(c.seqs, vec![8]);
        assert!(c.message.contains("cannot undo seq 7"));
    }

    #[test]
    fn lists_are_capped() {
        let concurrent: Vec<(i64, Vec<String>)> = (1..=100)
            .map(|s| (s, vec!["site/name".to_string()]))
            .collect();
        let c = check_field_conflicts(&["site/name".to_string()], &concurrent, 0, 100).unwrap_err();
        assert_eq!(c.seqs.len(), LIST_CAP);
    }
}
