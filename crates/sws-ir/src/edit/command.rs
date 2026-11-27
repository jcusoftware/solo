//! Command batch, command, error and transaction types for the editor reducer.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{IrStyleRule, Node, Page};

/// Current (and only) command batch format version. Independent of
/// `SiteDocument::schema_version`.
pub const COMMAND_BATCH_VERSION: u32 = 1;

/// Upper bound on commands in one batch (keeps Wasm memory and inverse size bounded).
pub const MAX_BATCH_COMMANDS: usize = 1000;

/// Stable, machine-readable error codes (snake_case on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidJson,
    UnsupportedVersion,
    EmptyBatch,
    BatchTooLarge,
    NotUpgraded,
    UnsupportedUidScheme,
    MissingUid,
    InvalidUid,
    DuplicateUid,
    DuplicateId,
    InvalidDocument,
    NotFound,
    AlreadyExists,
    InvalidValue,
    InvalidIndex,
    InvalidContainer,
    MoveIntoSelf,
    WrongNodeKind,
    LossyDocument,
    HistoryCorrupt,
}

impl ErrorCode {
    /// The wire string for this code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::UnsupportedVersion => "unsupported_version",
            Self::EmptyBatch => "empty_batch",
            Self::BatchTooLarge => "batch_too_large",
            Self::NotUpgraded => "not_upgraded",
            Self::UnsupportedUidScheme => "unsupported_uid_scheme",
            Self::MissingUid => "missing_uid",
            Self::InvalidUid => "invalid_uid",
            Self::DuplicateUid => "duplicate_uid",
            Self::DuplicateId => "duplicate_id",
            Self::InvalidDocument => "invalid_document",
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::InvalidValue => "invalid_value",
            Self::InvalidIndex => "invalid_index",
            Self::InvalidContainer => "invalid_container",
            Self::MoveIntoSelf => "move_into_self",
            Self::WrongNodeKind => "wrong_node_kind",
            Self::LossyDocument => "lossy_document",
            Self::HistoryCorrupt => "history_corrupt",
        }
    }
}

/// Typed editor error. `command_index` names the failing command of a batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_index: Option<usize>,
    /// Addresses (IDs or document paths) the error refers to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl CommandError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            command_index: None,
            paths: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_paths(mut self, paths: Vec<String>) -> Self {
        self.paths = paths;
        self
    }

    #[must_use]
    pub fn at(mut self, index: usize) -> Self {
        self.command_index = Some(index);
        self
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)?;
        if let Some(idx) = self.command_index {
            write!(f, " (command {idx})")?;
        }
        if !self.paths.is_empty() {
            write!(f, " [{}]", self.paths.join(", "))?;
        }
        Ok(())
    }
}

impl std::error::Error for CommandError {}

/// Address of an ordered node list that can receive or lose nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContainerRef {
    /// `pages[id].body`.
    PageBody { page_id: String },
    /// `components[name].template`.
    ComponentTemplate { name: String },
    /// A child list of the node with this `uid`. `slot` is one of `children`
    /// (element, default), `fallback` (slot_ref, default), `then_nodes`,
    /// `else_nodes` (conditional), `body` (loop, default) or `slots.<name>`
    /// (component_instance).
    Node {
        uid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot: Option<String>,
    },
}

/// One editor command. Nodes are addressed by `uid`, never by array index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    SetSiteMeta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locale: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
    },
    SetColorToken {
        key: String,
        hex: String,
    },
    RemoveColorToken {
        key: String,
    },
    SetFontToken {
        slot: String,
        stack: String,
    },
    /// Unset the optional `heading` or `mono` font slot.
    ClearFontToken {
        slot: String,
    },
    SetDataValue {
        key: String,
        value: Value,
    },
    RemoveDataValue {
        key: String,
    },
    /// Replace the rule with the same id in place, else append.
    UpsertStyleRule {
        rule: IrStyleRule,
    },
    InsertStyleRule {
        rule: IrStyleRule,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    RemoveStyleRule {
        id: String,
    },
    InsertPage {
        page: Page,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Replace the page with the same id in place, else append.
    UpsertPage {
        page: Page,
    },
    RemovePage {
        id: String,
    },
    /// Move a page so it ends at `index` in the page order.
    MovePage {
        id: String,
        index: usize,
    },
    InsertNode {
        container: ContainerRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        /// Whole subtree; every node must carry a unique `uid`.
        node: Node,
    },
    RemoveNode {
        uid: String,
    },
    /// Move a node (with its subtree) so it ends at `index` of `container`
    /// (`index` counts positions after the node is detached; `None` appends).
    MoveNode {
        uid: String,
        container: ContainerRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    SetAttr {
        uid: String,
        attr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
    /// Set the value of a `text` node (non-destructive: siblings are untouched).
    SetText {
        uid: String,
        value: String,
    },
    SetTag {
        uid: String,
        tag: String,
    },
    SetClasses {
        uid: String,
        classes: Vec<String>,
    },
    SetStyleRefs {
        uid: String,
        style_refs: Vec<String>,
    },
}

impl Command {
    /// True for commands that overwrite absolute values and are idempotent, so
    /// consecutive transactions touching identical fields can be compacted.
    #[must_use]
    pub const fn is_overwrite(&self) -> bool {
        matches!(
            self,
            Self::SetSiteMeta { .. }
                | Self::SetColorToken { .. }
                | Self::SetFontToken { .. }
                | Self::SetDataValue { .. }
                | Self::UpsertStyleRule { .. }
                | Self::SetAttr { .. }
                | Self::SetText { .. }
                | Self::SetTag { .. }
                | Self::SetClasses { .. }
                | Self::SetStyleRefs { .. }
        )
    }
}

/// Versioned, ordered batch of commands applied atomically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandBatch {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Consecutive batches sharing a non-empty key merge into one undo step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_key: Option<String>,
    pub commands: Vec<Command>,
}

impl CommandBatch {
    #[must_use]
    pub fn new(commands: Vec<Command>) -> Self {
        Self {
            version: COMMAND_BATCH_VERSION,
            label: None,
            coalesce_key: None,
            commands,
        }
    }

    /// Check version and size limits.
    pub fn validate(&self) -> Result<(), CommandError> {
        if self.version != COMMAND_BATCH_VERSION {
            return Err(CommandError::new(
                ErrorCode::UnsupportedVersion,
                format!(
                    "command batch version {} is not supported (expected {COMMAND_BATCH_VERSION})",
                    self.version
                ),
            ));
        }
        if self.commands.is_empty() {
            return Err(CommandError::new(
                ErrorCode::EmptyBatch,
                "command batch has no commands",
            ));
        }
        if self.commands.len() > MAX_BATCH_COMMANDS {
            return Err(CommandError::new(
                ErrorCode::BatchTooLarge,
                format!("command batch exceeds {MAX_BATCH_COMMANDS} commands"),
            ));
        }
        Ok(())
    }
}

/// Result of applying one batch: the replayable forward commands, the exact
/// inverse (in application order) and the conflict-addressable touched fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transaction {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_key: Option<String>,
    pub forward: Vec<Command>,
    pub inverse: Vec<Command>,
    /// Sorted, de-duplicated touched-field paths (see `docs/EDITOR_COMMANDS.md`).
    pub touched: Vec<String>,
}

impl Transaction {
    /// The forward commands as a batch (redo).
    #[must_use]
    pub fn forward_batch(&self) -> CommandBatch {
        CommandBatch::new(self.forward.clone())
    }

    /// The inverse commands as a batch (undo).
    #[must_use]
    pub fn inverse_batch(&self) -> CommandBatch {
        CommandBatch::new(self.inverse.clone())
    }

    fn compactable_with(&self, next: &Self) -> bool {
        self.touched == next.touched
            && self.forward.iter().all(Command::is_overwrite)
            && next.forward.iter().all(Command::is_overwrite)
    }

    /// True when [`Transaction::coalesce`] with `next` stays within
    /// [`MAX_BATCH_COMMANDS`] for both the forward and the inverse list, so the
    /// merged step can still be replayed by undo and redo.
    #[must_use]
    pub fn can_coalesce(&self, next: &Self) -> bool {
        if self.compactable_with(next) {
            return true;
        }
        self.forward.len() + next.forward.len() <= MAX_BATCH_COMMANDS
            && self.inverse.len() + next.inverse.len() <= MAX_BATCH_COMMANDS
    }

    /// Merge a later transaction into this one so one undo reverts both.
    ///
    /// When every command of both is an idempotent overwrite and both touch
    /// exactly the same fields, the result is compacted to the later forward
    /// commands and the earlier inverse; otherwise forward commands are
    /// concatenated and inverses are chained in reverse.
    #[must_use]
    pub fn coalesce(mut self, next: Self) -> Self {
        if self.compactable_with(&next) {
            self.forward = next.forward;
        } else {
            self.forward.extend(next.forward);
            let mut inverse = next.inverse;
            inverse.extend(std::mem::take(&mut self.inverse));
            self.inverse = inverse;
            let merged: BTreeSet<String> = self.touched.drain(..).chain(next.touched).collect();
            self.touched = merged.into_iter().collect();
        }
        if next.label.is_some() {
            self.label = next.label;
        }
        self
    }
}

/// True when two touched-field paths address overlapping fields: equal, or one
/// is an ancestor (`/`-separated) of the other. Conservative by design.
#[must_use]
pub fn paths_conflict(a: &str, b: &str) -> bool {
    a == b
        || (a.len() < b.len() && b.starts_with(a) && b.as_bytes()[a.len()] == b'/')
        || (b.len() < a.len() && a.starts_with(b) && a.as_bytes()[b.len()] == b'/')
}

/// True when any path of `a` conflicts with any path of `b`.
#[must_use]
pub fn touched_conflict(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.iter().any(|y| paths_conflict(x, y)))
}
