//! Undo/redo history over the canonical reducer.

use super::command::{CommandBatch, CommandError, ErrorCode, Transaction};
use super::ids::check_editable;
use super::reducer::{apply_batch, apply_batch_restore};
use crate::types::SiteDocument;

/// Maximum undo steps retained by [`History::new`].
pub const DEFAULT_HISTORY_LIMIT: usize = 200;

/// An editable document with deterministic undo/redo.
///
/// Every state change goes through [`apply_batch`], so undo and redo replay
/// recorded commands exactly. A failed operation leaves the history untouched.
#[derive(Debug, Clone)]
pub struct History {
    doc: SiteDocument,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    limit: usize,
    /// Set by undo/redo so the next batch never merges into an older step.
    sealed: bool,
}

impl History {
    /// Open `doc` for editing; refuses documents that fail [`check_editable`].
    pub fn new(doc: SiteDocument) -> Result<Self, CommandError> {
        Self::with_limit(doc, DEFAULT_HISTORY_LIMIT)
    }

    /// Like [`History::new`] with an explicit undo-step cap (minimum 1).
    pub fn with_limit(doc: SiteDocument, limit: usize) -> Result<Self, CommandError> {
        check_editable(&doc)?;
        Ok(Self {
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
            limit: limit.max(1),
            sealed: false,
        })
    }

    #[must_use]
    pub fn document(&self) -> &SiteDocument {
        &self.doc
    }

    #[must_use]
    pub fn into_document(self) -> SiteDocument {
        self.doc
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// Apply a batch as a new undo step (clearing redo). A batch whose
    /// `coalesce_key` equals that of the previous step, with no undo/redo in
    /// between, merges into that step unless the merged step would exceed
    /// `MAX_BATCH_COMMANDS` (then a new step starts). Returns the batch's own transaction.
    pub fn apply(&mut self, batch: &CommandBatch) -> Result<Transaction, CommandError> {
        let (next, tx) = apply_batch(&self.doc, batch)?;
        self.doc = next;
        self.redo.clear();
        let merge = !self.sealed
            && tx.coalesce_key.is_some()
            && self
                .undo
                .last()
                .is_some_and(|top| top.coalesce_key == tx.coalesce_key && top.can_coalesce(&tx));
        if merge && let Some(top) = self.undo.pop() {
            self.undo.push(top.coalesce(tx.clone()));
        } else {
            self.undo.push(tx.clone());
            if self.undo.len() > self.limit {
                self.undo.remove(0);
            }
        }
        self.sealed = false;
        Ok(tx)
    }

    /// Revert the latest step. Returns `Ok(None)` when there is nothing to undo.
    pub fn undo(&mut self) -> Result<Option<Transaction>, CommandError> {
        let Some(tx) = self.undo.pop() else {
            return Ok(None);
        };
        match apply_batch_restore(&self.doc, &tx.inverse_batch()) {
            Ok((next, _)) => {
                self.doc = next;
                self.redo.push(tx.clone());
                self.sealed = true;
                Ok(Some(tx))
            }
            Err(e) => {
                self.undo.push(tx);
                Err(history_corrupt(&e))
            }
        }
    }

    /// Re-apply the latest undone step. Returns `Ok(None)` when there is nothing to redo.
    pub fn redo(&mut self) -> Result<Option<Transaction>, CommandError> {
        let Some(tx) = self.redo.pop() else {
            return Ok(None);
        };
        match apply_batch(&self.doc, &tx.forward_batch()) {
            Ok((next, _)) => {
                self.doc = next;
                self.undo.push(tx.clone());
                self.sealed = true;
                Ok(Some(tx))
            }
            Err(e) => {
                self.redo.push(tx);
                Err(history_corrupt(&e))
            }
        }
    }
}

fn history_corrupt(cause: &CommandError) -> CommandError {
    CommandError::new(
        ErrorCode::HistoryCorrupt,
        format!("recorded transaction no longer applies: {cause}"),
    )
}
