//! The session engine: a pure state machine over a [`Store`].
//!
//! It owns the live `SiteDocument` and the command log of one site and decides
//! every outcome (apply, duplicate, rebase, conflict, undo, checkpoint,
//! presence, compaction, alarm planning). It performs no I/O of its own and
//! never reimplements command reduction: every document change goes through
//! `sws_ir::edit::{apply_batch, apply_batch_restore}`.
//!
//! Callers (the Durable Object) authorize first and call the engine in one
//! synchronous block, so a commit and its broadcast cannot interleave with
//! another message.

use sws_ir::SiteDocument;
use sws_ir::edit::{
    CommandBatch, CommandError, Transaction, apply_batch, apply_batch_restore, canonical_json,
    check_editable, lossy_error, lossy_paths, source_issues,
};

use super::protocol::CheckpointReason;
use super::rebase::{self, Conflict, ConflictKind};
use super::sha256::sha256_hex;
use super::store::{
    EntryKind, LogEntry, Meta, OpRecord, PendingRevision, PresenceRow, SnapshotRow, Store,
    StoreError,
};
use super::{
    CHECKPOINT_EVERY, LOG_RETAIN, MAX_DOCUMENT_BYTES, MAX_OPS, MAX_UNCHECKPOINTED, OP_TTL_MS,
    PRESENCE_MIN_INTERVAL_MS, REPLAY_MAX,
};

/// Infrastructure failure (not a client mistake): the object should surface a
/// generic internal error and log it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    Store(StoreError),
    Corrupt(String),
    /// The object was addressed with a different workspace/site than it holds.
    IdentityMismatch,
}

impl From<StoreError> for EngineError {
    fn from(e: StoreError) -> Self {
        EngineError::Store(e)
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Store(e) => write!(f, "{e}"),
            EngineError::Corrupt(m) => write!(f, "session state is corrupt: {m}"),
            EngineError::IdentityMismatch => write!(f, "session identity mismatch"),
        }
    }
}

impl std::error::Error for EngineError {}

/// A client-visible refusal. `code()` is stable; conflicts have their own message.
#[derive(Debug, Clone, PartialEq)]
pub enum Rejection {
    NotSeeded,
    AlreadySeeded,
    InvalidBase {
        base_seq: i64,
        head_seq: i64,
    },
    OpIdReused,
    DocumentTooLarge {
        bytes: usize,
    },
    /// The reducer refused the batch (`command_index` names the command).
    Command(CommandError),
    /// A seed document that is not editable or would lose data on save.
    InvalidDocument(CommandError),
    NotFound(String),
    UndoNotOwner,
    CheckpointBacklog,
    RateLimited,
    /// Session storage failed; the client may retry.
    Unavailable(String),
    Conflict(Conflict),
}

impl Rejection {
    pub fn code(&self) -> &'static str {
        match self {
            Rejection::NotSeeded => "not_seeded",
            Rejection::AlreadySeeded => "already_seeded",
            Rejection::InvalidBase { .. } => "invalid_base",
            Rejection::OpIdReused => "op_id_reused",
            Rejection::DocumentTooLarge { .. } => "document_too_large",
            Rejection::Command(_) => "command_error",
            Rejection::InvalidDocument(_) => "invalid_document",
            Rejection::NotFound(_) => "not_found",
            Rejection::UndoNotOwner => "undo_not_owner",
            Rejection::CheckpointBacklog => "checkpoint_backlog",
            Rejection::RateLimited => "rate_limited",
            Rejection::Unavailable(_) => "unavailable",
            Rejection::Conflict(_) => "conflict",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Rejection::NotSeeded => {
                "this site has no document yet; a publisher or owner must send `seed` first".into()
            }
            Rejection::AlreadySeeded => "this site already has a document".into(),
            Rejection::InvalidBase { base_seq, head_seq } => format!(
                "base_seq {base_seq} is ahead of the document (seq {head_seq}); send `sync` to resync"
            ),
            Rejection::OpIdReused => {
                "this op_id was already used by another user; choose a fresh op_id".into()
            }
            Rejection::DocumentTooLarge { bytes } => {
                format!("the document would be {bytes} bytes; the limit is {MAX_DOCUMENT_BYTES}")
            }
            Rejection::Command(e) | Rejection::InvalidDocument(e) => e.to_string(),
            Rejection::NotFound(m) => m.clone(),
            Rejection::UndoNotOwner => {
                "you can only undo your own changes; make the edit as a new batch".into()
            }
            Rejection::CheckpointBacklog => format!(
                "the registry checkpoint is still pending and {MAX_UNCHECKPOINTED} commands are \
                 unsaved; retry shortly"
            ),
            Rejection::RateLimited => "presence updates are limited; slow down".into(),
            Rejection::Unavailable(m) => m.clone(),
            Rejection::Conflict(c) => c.message.clone(),
        }
    }
}

/// Who is acting. Authorization has already been decided by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub user_id: i64,
    pub client_id: String,
}

/// A command that is now (or already was) part of the log.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    pub seq: i64,
    pub duplicate: bool,
    pub rebased: bool,
    /// The new log entry; `None` for a duplicate (already delivered once).
    pub entry: Option<LogEntry>,
}

pub type Submit = Result<Committed, Rejection>;

#[derive(Debug, Clone, PartialEq)]
pub enum CheckpointOutcome {
    /// A new checkpoint was taken; the D1 revision row is pending.
    Taken(PendingRevision),
    /// An earlier checkpoint is still waiting for its D1 row.
    Pending(PendingRevision),
    /// Nothing changed since the last checkpoint.
    Unchanged { seq: i64 },
}

/// What `sync` should send.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncPlan {
    Replay(Vec<LogEntry>),
    Full,
}

/// Retry delay for an unresolved D1 checkpoint write: 2 s doubling, 5 min cap.
pub fn backoff_ms(attempts: u32) -> i64 {
    (2_000i64 << attempts.min(7)).min(300_000)
}

/// `ck-` plus 24 hex characters: deterministic per (workspace, site, seq, hash),
/// so the same checkpoint always carries the same D1 primary key (retry-safe).
pub fn revision_id(workspace_id: &str, site_id: &str, seq: i64, ir_sha256: &str) -> String {
    let digest = sha256_hex(format!("{workspace_id}\0{site_id}\0{seq}\0{ir_sha256}").as_bytes());
    format!("ck-{}", &digest[..24])
}

pub struct Engine<S: Store> {
    store: S,
    meta: Meta,
    doc: Option<SiteDocument>,
    head: i64,
}

impl<S: Store> Engine<S> {
    /// Rebuild from storage: latest snapshot plus a replay of the log tail.
    /// This is exactly what runs when a hibernated object wakes up.
    pub fn open(mut store: S, workspace_id: &str, site_id: &str) -> Result<Self, EngineError> {
        let mut meta = store.load_meta()?;
        if meta.workspace_id.is_empty() {
            meta.workspace_id = workspace_id.to_string();
            meta.site_id = site_id.to_string();
            store.save_meta(&meta)?;
        } else if meta.workspace_id != workspace_id || meta.site_id != site_id {
            return Err(EngineError::IdentityMismatch);
        }
        let mut doc = None;
        let mut head = 0;
        if let Some(snap) = store.latest_snapshot()? {
            let mut d: SiteDocument = serde_json::from_str(&snap.doc)
                .map_err(|e| EngineError::Corrupt(format!("snapshot {}: {e}", snap.seq)))?;
            head = snap.seq;
            loop {
                let page = store.log_after(head, 100)?;
                if page.is_empty() {
                    break;
                }
                for e in page {
                    if e.seq != head + 1 {
                        return Err(EngineError::Corrupt(format!(
                            "log gap: expected seq {}, found {}",
                            head + 1,
                            e.seq
                        )));
                    }
                    let tx: Transaction = serde_json::from_str(&e.tx_json)
                        .map_err(|err| EngineError::Corrupt(format!("seq {}: {err}", e.seq)))?;
                    // Restore mode skips only new-input grammar checks (it replays
                    // values the document already held); results are identical.
                    d = apply_batch_restore(&d, &tx.forward_batch())
                        .map_err(|err| EngineError::Corrupt(format!("seq {}: {err}", e.seq)))?
                        .0;
                    head = e.seq;
                }
            }
            if store.log_max_seq()? > head {
                return Err(EngineError::Corrupt("log is ahead of its replay".into()));
            }
            doc = Some(d);
        }
        Ok(Self {
            store,
            meta,
            doc,
            head,
        })
    }

    #[cfg(test)]
    pub fn into_store(self) -> S {
        self.store
    }

    pub fn head_seq(&self) -> i64 {
        self.head
    }

    pub fn seeded(&self) -> bool {
        self.doc.is_some()
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn document(&self) -> Option<&SiteDocument> {
        self.doc.as_ref()
    }

    /// Canonical document JSON text (what a client loads and a checkpoint hashes).
    pub fn document_text(&self) -> Option<String> {
        self.doc.as_ref().map(canonical_json)
    }

    pub fn checkpoint_seq(&self) -> Option<i64> {
        self.meta.last_checkpoint_seq
    }

    // --- Seed -----------------------------------------------------------

    /// Install the first document. Refused unless the site is empty. The text
    /// must be lossless and editable (same gate as `solo edit`).
    pub fn seed(&mut self, document_json: &str, now_ms: i64) -> Result<(), Rejection> {
        if self.doc.is_some() {
            return Err(Rejection::AlreadySeeded);
        }
        let invalid = |m: &str| {
            Rejection::InvalidDocument(CommandError::new(
                sws_ir::edit::ErrorCode::InvalidJson,
                m.to_string(),
            ))
        };
        let raw: serde_json::Value = serde_json::from_str(document_json)
            .map_err(|e| invalid(&format!("invalid IR JSON: {e}")))?;
        let doc: SiteDocument = serde_json::from_value(raw.clone())
            .map_err(|e| invalid(&format!("invalid SiteDocument JSON: {e}")))?;
        let mut paths = lossy_paths(&raw, &doc);
        paths.extend(source_issues(document_json));
        lossy_error(paths).map_err(Rejection::InvalidDocument)?;
        check_editable(&doc).map_err(Rejection::InvalidDocument)?;
        let text = canonical_json(&doc);
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(Rejection::DocumentTooLarge { bytes: text.len() });
        }
        let snap = SnapshotRow {
            seq: 0,
            sha256: sha256_hex(text.as_bytes()),
            doc: text,
        };
        self.store
            .put_snapshot(&snap, now_ms)
            .map_err(|e| store_failure(&e))?;
        self.meta.snapshot_seq = 0;
        self.store
            .save_meta(&self.meta)
            .map_err(|e| store_failure(&e))?;
        self.doc = Some(doc);
        self.head = 0;
        Ok(())
    }

    // --- Commands -------------------------------------------------------

    fn duplicate(
        &self,
        actor: &Actor,
        op_id: &str,
        now_ms: i64,
    ) -> Result<Option<Committed>, Rejection> {
        match self
            .store
            .op_get(op_id, now_ms)
            .map_err(|e| store_failure(&e))?
        {
            None => Ok(None),
            Some(op) if op.user_id != actor.user_id => Err(Rejection::OpIdReused),
            Some(op) => Ok(Some(Committed {
                seq: op.seq,
                duplicate: true,
                rebased: op.rebased,
                entry: None,
            })),
        }
    }

    fn guard_backlog(&self) -> Result<(), Rejection> {
        let base = self.meta.last_checkpoint_seq.unwrap_or(0);
        if self.meta.pending.is_some() && self.head - base >= MAX_UNCHECKPOINTED {
            return Err(Rejection::CheckpointBacklog);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn commit(
        &mut self,
        actor: &Actor,
        op_id: &str,
        kind: EntryKind,
        target_seq: Option<i64>,
        base_seq: i64,
        rebased: bool,
        new_doc: SiteDocument,
        tx: &Transaction,
        now_ms: i64,
    ) -> Submit {
        let text = canonical_json(&new_doc);
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(Rejection::DocumentTooLarge { bytes: text.len() });
        }
        let tx_json = serde_json::to_string(tx).map_err(|e| {
            Rejection::Command(CommandError::new(
                sws_ir::edit::ErrorCode::InvalidDocument,
                format!("transaction is not encodable: {e}"),
            ))
        })?;
        let seq = self.head + 1;
        let entry = LogEntry {
            seq,
            op_id: op_id.to_string(),
            user_id: actor.user_id,
            client_id: actor.client_id.clone(),
            kind,
            target_seq,
            base_seq,
            rebased,
            touched: tx.touched.clone(),
            tx_json,
            ts_ms: now_ms,
        };
        let op = OpRecord {
            op_id: op_id.to_string(),
            user_id: actor.user_id,
            seq,
            rebased,
            expires_ms: now_ms.saturating_add(OP_TTL_MS),
        };
        // One synchronous turn on DO SQL: the log row and its op id commit together.
        self.store
            .append(&entry, &op)
            .map_err(|e| store_failure(&e))?;
        self.doc = Some(new_doc);
        self.head = seq;
        Ok(Committed {
            seq,
            duplicate: false,
            rebased,
            entry: Some(entry),
        })
    }

    /// Apply a client batch. See [`rebase`] for the stale-batch rules.
    pub fn submit_batch(
        &mut self,
        actor: &Actor,
        op_id: &str,
        base_seq: i64,
        batch: &CommandBatch,
        now_ms: i64,
    ) -> Submit {
        if let Some(dup) = self.duplicate(actor, op_id, now_ms)? {
            return Ok(dup);
        }
        let doc = self.doc.as_ref().ok_or(Rejection::NotSeeded)?;
        self.guard_backlog()?;
        let head = self.head;
        if base_seq > head {
            return Err(Rejection::InvalidBase {
                base_seq,
                head_seq: head,
            });
        }
        let stale = base_seq < head;
        let concurrent = if stale {
            if base_seq < self.meta.compacted_through {
                return Err(Rejection::Conflict(too_old(base_seq, head, "batch")));
            }
            rebase::check_rebaseable(&batch.commands, base_seq, head)
                .map_err(Rejection::Conflict)?;
            let rows = self
                .store
                .touched_after(base_seq)
                .map_err(|e| store_failure(&e))?;
            if rows.len() as i64 != head - base_seq {
                return Err(Rejection::Conflict(too_old(base_seq, head, "batch")));
            }
            rows
        } else {
            Vec::new()
        };
        let (new_doc, tx) = match apply_batch(doc, batch) {
            Ok(r) => r,
            Err(e) if stale => {
                return Err(Rejection::Conflict(Conflict {
                    kind: ConflictKind::TargetChanged,
                    message: format!(
                        "the batch is based on seq {base_seq} but no longer applies at seq {head}: {e}. \
                         Apply the newer entries (send `sync` with since_seq {base_seq}) and resend."
                    ),
                    head_seq: head,
                    seqs: Vec::new(),
                    paths: e.paths.clone(),
                }));
            }
            Err(e) => return Err(Rejection::Command(e)),
        };
        if stale {
            rebase::check_field_conflicts(&tx.touched, &concurrent, base_seq, head)
                .map_err(Rejection::Conflict)?;
        }
        self.commit(
            actor,
            op_id,
            EntryKind::Batch,
            None,
            base_seq,
            stale,
            new_doc,
            &tx,
            now_ms,
        )
    }

    /// Collaborative undo: append a NEW entry applying the target's recorded
    /// inverse. History is never rewound or rewritten.
    pub fn submit_undo(
        &mut self,
        actor: &Actor,
        op_id: &str,
        target_seq: i64,
        now_ms: i64,
    ) -> Submit {
        if let Some(dup) = self.duplicate(actor, op_id, now_ms)? {
            return Ok(dup);
        }
        let doc = self.doc.as_ref().ok_or(Rejection::NotSeeded)?;
        self.guard_backlog()?;
        let head = self.head;
        if target_seq > head {
            return Err(Rejection::NotFound(format!(
                "seq {target_seq} does not exist (head is {head})"
            )));
        }
        let too_old = |why: &str| {
            Rejection::Conflict(Conflict {
                kind: ConflictKind::UndoTooOld,
                message: format!(
                    "seq {target_seq} {why}; it can no longer be undone. Make the change as a new batch."
                ),
                head_seq: head,
                seqs: vec![target_seq],
                paths: Vec::new(),
            })
        };
        if target_seq <= self.meta.compacted_through {
            return Err(too_old("is older than the retained command log"));
        }
        let target = self
            .store
            .log_get(target_seq)
            .map_err(|e| store_failure(&e))?
            .ok_or_else(|| too_old("is not in the command log"))?;
        if target.user_id != actor.user_id {
            return Err(Rejection::UndoNotOwner);
        }
        let later = self
            .store
            .touched_after(target_seq)
            .map_err(|e| store_failure(&e))?;
        if later.len() as i64 != head - target_seq {
            return Err(too_old("is not fully retained in the command log"));
        }
        rebase::check_undo(target_seq, &target.touched, &later, head)
            .map_err(Rejection::Conflict)?;
        let tx: Transaction = serde_json::from_str(&target.tx_json).map_err(|e| {
            Rejection::Command(CommandError::new(
                sws_ir::edit::ErrorCode::HistoryCorrupt,
                format!("recorded transaction for seq {target_seq} is unreadable: {e}"),
            ))
        })?;
        let (new_doc, undo_tx) = apply_batch_restore(doc, &tx.inverse_batch()).map_err(|e| {
            Rejection::Conflict(Conflict {
                kind: ConflictKind::UndoIncompatible,
                message: format!(
                    "cannot undo seq {target_seq}: its inverse no longer applies ({e}). \
                     Make the change as a new batch."
                ),
                head_seq: head,
                seqs: vec![target_seq],
                paths: e.paths.clone(),
            })
        })?;
        self.commit(
            actor,
            op_id,
            EntryKind::Undo,
            Some(target_seq),
            head,
            false,
            new_doc,
            &undo_tx,
            now_ms,
        )
    }

    // --- Sync -----------------------------------------------------------

    /// Decide between a small replay and the full state.
    pub fn sync_plan(&self, since_seq: Option<i64>) -> Result<SyncPlan, EngineError> {
        let Some(since) = since_seq else {
            return Ok(SyncPlan::Full);
        };
        if since > self.head
            || since < self.meta.compacted_through
            || self.head - since > REPLAY_MAX
        {
            return Ok(SyncPlan::Full);
        }
        let entries = self.store.log_after(since, REPLAY_MAX as usize)?;
        if entries.len() as i64 != self.head - since {
            return Ok(SyncPlan::Full);
        }
        Ok(SyncPlan::Replay(entries))
    }

    // --- Checkpoints ------------------------------------------------------

    /// True when `CHECKPOINT_EVERY` commands have accumulated and no D1 write
    /// is outstanding.
    pub fn checkpoint_due(&self) -> bool {
        self.doc.is_some()
            && self.meta.pending.is_none()
            && self.head - self.meta.last_checkpoint_seq.unwrap_or(0) >= CHECKPOINT_EVERY
    }

    pub fn pending(&self) -> Option<&PendingRevision> {
        self.meta.pending.as_ref()
    }

    /// Snapshot the head locally and queue exactly one D1 revision row.
    /// Idempotent: nothing new since the last checkpoint is `Unchanged`; an
    /// outstanding checkpoint is returned as `Pending` (flush it first).
    pub fn take_checkpoint(
        &mut self,
        created_by: i64,
        reason: CheckpointReason,
        now_ms: i64,
    ) -> Result<CheckpointOutcome, Rejection> {
        let Some(doc) = self.doc.as_ref() else {
            return Err(Rejection::NotSeeded);
        };
        if let Some(p) = &self.meta.pending {
            return Ok(CheckpointOutcome::Pending(p.clone()));
        }
        if self.meta.last_checkpoint_seq == Some(self.head) {
            return Ok(CheckpointOutcome::Unchanged { seq: self.head });
        }
        let text = canonical_json(doc);
        let ir_sha256 = sha256_hex(text.as_bytes());
        let pending = PendingRevision {
            seq: self.head,
            revision_id: revision_id(
                &self.meta.workspace_id,
                &self.meta.site_id,
                self.head,
                &ir_sha256,
            ),
            ir_sha256: ir_sha256.clone(),
            byte_size: text.len() as i64,
            created_by,
            reason: reason.as_str().to_string(),
            attempts: 0,
        };
        let snap = SnapshotRow {
            seq: self.head,
            doc: text,
            sha256: ir_sha256,
        };
        // Snapshot, then meta (both in this synchronous turn).
        self.store
            .put_snapshot(&snap, now_ms)
            .map_err(|e| store_failure(&e))?;
        self.meta.snapshot_seq = self.head;
        self.meta.last_checkpoint_seq = Some(self.head);
        self.meta.pending = Some(pending.clone());
        self.store
            .save_meta(&self.meta)
            .map_err(|e| store_failure(&e))?;
        Ok(CheckpointOutcome::Taken(pending))
    }

    /// The D1 row for `seq` is confirmed written (or already existed).
    pub fn mark_revision_written(&mut self, seq: i64) -> Result<bool, EngineError> {
        if self.meta.pending.as_ref().is_some_and(|p| p.seq == seq) {
            self.meta.pending = None;
            self.store.save_meta(&self.meta)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Count a failed D1 attempt (drives the alarm backoff).
    pub fn note_revision_failure(&mut self) -> Result<(), EngineError> {
        if let Some(p) = self.meta.pending.as_mut() {
            p.attempts = p.attempts.saturating_add(1);
            self.store.save_meta(&self.meta)?;
        }
        Ok(())
    }

    // --- Compaction (alarm) -------------------------------------------------

    fn horizon(&self) -> i64 {
        (self.meta.snapshot_seq - LOG_RETAIN).max(0)
    }

    pub fn needs_compaction(&self) -> bool {
        self.horizon() > self.meta.compacted_through
    }

    /// Drop log entries older than the retained window below the latest
    /// snapshot, and prune expired operation ids. Retry-safe: the delete is
    /// idempotent and the horizon only moves forward, so a crash between the
    /// two writes just repeats a no-op.
    pub fn compact(&mut self, now_ms: i64) -> Result<Option<i64>, EngineError> {
        self.store.prune_ops(now_ms, MAX_OPS)?;
        let h = self.horizon();
        if h <= self.meta.compacted_through {
            return Ok(None);
        }
        self.store.compact_log(h)?;
        self.meta.compacted_through = h;
        self.store.save_meta(&self.meta)?;
        Ok(Some(h))
    }

    // --- Presence -----------------------------------------------------------

    /// Set a client's soft presence; expires `ttl_ms` after the last update.
    pub fn presence_set(
        &mut self,
        client_id: &str,
        user_id: i64,
        state: String,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<(), Rejection> {
        if let Some(prev) = self
            .store
            .presence_get(client_id)
            .map_err(|e| store_failure(&e))?
            && now_ms.saturating_sub(prev.updated_ms) < PRESENCE_MIN_INTERVAL_MS
        {
            return Err(Rejection::RateLimited);
        }
        self.store
            .presence_put(&PresenceRow {
                client_id: client_id.to_string(),
                user_id,
                state,
                updated_ms: now_ms,
                expires_ms: now_ms.saturating_add(ttl_ms),
            })
            .map_err(|e| store_failure(&e))
    }

    pub fn presence_remove(&mut self, client_id: &str) -> Result<(), EngineError> {
        Ok(self.store.presence_delete(client_id)?)
    }

    /// Live (unexpired) presence rows.
    pub fn presence_list(&self, now_ms: i64) -> Result<Vec<PresenceRow>, EngineError> {
        Ok(self.store.presence_list(now_ms)?)
    }

    /// Remove expired rows; returns how many were removed.
    pub fn presence_prune(&mut self, now_ms: i64) -> Result<usize, EngineError> {
        Ok(self.store.presence_prune(now_ms)?)
    }

    // --- Alarm planning -----------------------------------------------------

    /// When the next alarm should fire (epoch ms), or `None` when nothing is
    /// outstanding: an unresolved D1 checkpoint (with backoff), compaction,
    /// presence expiry, operation-id expiry, and, while sockets are attached,
    /// the membership recheck sweep.
    pub fn next_alarm(
        &self,
        now_ms: i64,
        connections: usize,
        authz_recheck_ms: i64,
    ) -> Result<Option<i64>, EngineError> {
        let mut at: Vec<i64> = Vec::new();
        if let Some(p) = &self.meta.pending {
            at.push(now_ms + backoff_ms(p.attempts));
        }
        if self.needs_compaction() {
            at.push(now_ms + 1_000);
        }
        if let Some(t) = self.store.next_presence_expiry()? {
            at.push(t.max(now_ms + 100));
        }
        if let Some(t) = self.store.next_op_expiry()? {
            at.push(t.max(now_ms + 100));
        }
        if connections > 0 {
            at.push(now_ms + authz_recheck_ms);
        }
        Ok(at.into_iter().min())
    }
}

fn too_old(base_seq: i64, head: i64, what: &str) -> Conflict {
    Conflict {
        kind: ConflictKind::BaseTooOld,
        message: format!(
            "the {what} is based on seq {base_seq}, older than the retained command log \
             (head is {head}); send `sync` to reload the document and resend."
        ),
        head_seq: head,
        seqs: Vec::new(),
        paths: Vec::new(),
    }
}

/// Storage failures inside a client operation are infrastructure errors, but
/// the engine's client-facing API returns [`Rejection`]; map them to
/// `unavailable`, which the glue logs and the client may retry.
fn store_failure(e: &StoreError) -> Rejection {
    Rejection::Unavailable(format!(
        "session storage is temporarily unavailable ({e}); retry"
    ))
}
