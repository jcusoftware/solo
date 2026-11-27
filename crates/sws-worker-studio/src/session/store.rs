//! Storage seam for the session engine.
//!
//! [`Store`] is synchronous (Durable Object SQLite is) and models exactly the
//! tables in [`crate::session::sql`]. The wasm build implements it over DO SQL
//! (`do_store`); host tests use [`MemStore`], which can also be dropped and
//! rebuilt from its persisted state to model hibernation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session store: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Batch,
    Undo,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Batch => "batch",
            EntryKind::Undo => "undo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "batch" => Some(EntryKind::Batch),
            "undo" => Some(EntryKind::Undo),
            _ => None,
        }
    }
}

/// One applied command (a batch, or a collaborative undo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub seq: i64,
    pub op_id: String,
    pub user_id: i64,
    pub client_id: String,
    pub kind: EntryKind,
    /// For `Undo`: the entry whose inverse this applies.
    pub target_seq: Option<i64>,
    /// The sequence number the client based the batch on.
    pub base_seq: i64,
    pub rebased: bool,
    /// Touched fields of the transaction as applied (conflict addressing).
    pub touched: Vec<String>,
    /// The `sws_ir::edit::Transaction` as JSON (forward, inverse, touched).
    pub tx_json: String,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRow {
    pub seq: i64,
    /// Canonical document JSON (`sws_ir::edit::canonical_json`).
    pub doc: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRecord {
    pub op_id: String,
    pub user_id: i64,
    pub seq: i64,
    pub rebased: bool,
    pub expires_ms: i64,
    /// Hash of the request kind and content that produced this op id, so a
    /// reused id carrying a different request is refused, not silently acked.
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceRow {
    pub client_id: String,
    pub user_id: i64,
    /// Compact JSON object, at most `PRESENCE_STATE_MAX_BYTES`.
    pub state: String,
    pub updated_ms: i64,
    pub expires_ms: i64,
}

/// A checkpoint whose D1 `site_revisions` row is not confirmed yet. Persisted so
/// a hibernation or crash between the snapshot and the D1 write retries safely
/// (the id is deterministic and the insert is `ON CONFLICT (id) DO NOTHING`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRevision {
    pub seq: i64,
    pub revision_id: String,
    pub ir_sha256: String,
    pub byte_size: i64,
    pub created_by: i64,
    /// `auto`, `save` or `publish`.
    pub reason: String,
    pub attempts: u32,
}

/// A checkpoint revision and the snapshot sequence that holds its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionRef {
    pub seq: i64,
    pub revision_id: String,
}

/// Small durable state of the session, stored as one JSON row.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Meta {
    pub workspace_id: String,
    pub site_id: String,
    /// `seq` of the latest snapshot (0 = the seed).
    pub snapshot_seq: i64,
    /// Log entries with `seq <= compacted_through` are gone.
    pub compacted_through: i64,
    pub last_checkpoint_seq: Option<i64>,
    /// `last_checkpoint_seq` before the pending checkpoint was taken (restored
    /// when a permanently failing pending row is abandoned).
    #[serde(default)]
    pub prev_checkpoint_seq: Option<i64>,
    /// Head seq of the latest checkpoint dropped as permanently unwritable. The
    /// next automatic checkpoint is due `CHECKPOINT_EVERY` commands after it, so
    /// a persistent failure is retried at that cadence, never once per edit.
    #[serde(default)]
    pub abandoned_seq: Option<i64>,
    /// The latest checkpoint whose D1 row is confirmed written. Its id is what an
    /// `unchanged` save/publish answers with.
    #[serde(default)]
    pub last_revision: Option<RevisionRef>,
    /// The latest confirmed publish checkpoint; its snapshot is retained so its
    /// bytes stay fetchable by `revision_id` (#64).
    #[serde(default)]
    pub last_publish: Option<RevisionRef>,
    pub pending: Option<PendingRevision>,
}

pub trait Store {
    fn load_meta(&self) -> StoreResult<Meta>;
    fn save_meta(&mut self, meta: &Meta) -> StoreResult<()>;

    fn latest_snapshot(&self) -> StoreResult<Option<SnapshotRow>>;
    fn snapshot_get(&self, seq: i64) -> StoreResult<Option<SnapshotRow>>;
    fn put_snapshot(&mut self, snap: &SnapshotRow, now_ms: i64) -> StoreResult<()>;
    /// Delete every snapshot older than `latest_seq` except the `keep` seqs (the
    /// latest confirmed checkpoint and the latest confirmed publish).
    fn prune_snapshots(&mut self, latest_seq: i64, keep: [Option<i64>; 2]) -> StoreResult<()>;
    /// True (once) if any storage statement failed since the last call. A failed
    /// call may have left earlier writes in place, so the caller must drop its
    /// in-memory state and rebuild from storage.
    fn take_failure(&mut self) -> bool {
        false
    }

    /// Highest `seq` in the log (0 when empty).
    fn log_max_seq(&self) -> StoreResult<i64>;
    fn log_after(&self, after_seq: i64, limit: usize) -> StoreResult<Vec<LogEntry>>;
    fn log_get(&self, seq: i64) -> StoreResult<Option<LogEntry>>;
    /// `(seq, touched)` for every entry after `after_seq`, ascending.
    fn touched_after(&self, after_seq: i64) -> StoreResult<Vec<(i64, Vec<String>)>>;
    /// Append the entry and record its operation id atomically.
    fn append(&mut self, entry: &LogEntry, op: &OpRecord) -> StoreResult<()>;
    fn compact_log(&mut self, through_seq: i64) -> StoreResult<()>;

    fn op_get(&self, op_id: &str, now_ms: i64) -> StoreResult<Option<OpRecord>>;
    fn prune_ops(&mut self, now_ms: i64, keep: i64) -> StoreResult<()>;
    fn next_op_expiry(&self) -> StoreResult<Option<i64>>;

    fn presence_get(&self, client_id: &str) -> StoreResult<Option<PresenceRow>>;
    fn presence_put(&mut self, row: &PresenceRow) -> StoreResult<()>;
    fn presence_delete(&mut self, client_id: &str) -> StoreResult<()>;
    fn presence_list(&self, now_ms: i64) -> StoreResult<Vec<PresenceRow>>;
    /// Delete expired rows; returns how many.
    fn presence_prune(&mut self, now_ms: i64) -> StoreResult<usize>;
    fn next_presence_expiry(&self) -> StoreResult<Option<i64>>;
}

/// In-memory [`Store`] for host tests. `Clone` copies the persisted state, which
/// is how tests model "the object was evicted and rebuilt from storage".
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct MemStore {
    pub meta: Option<Meta>,
    pub snapshots: std::collections::BTreeMap<i64, SnapshotRow>,
    pub log: std::collections::BTreeMap<i64, LogEntry>,
    pub ops: std::collections::BTreeMap<String, OpRecord>,
    pub presence: std::collections::BTreeMap<String, PresenceRow>,
    /// Fault injection: fail the Nth mutating statement from now (1 = the next).
    pub fail_in: Option<usize>,
    pub failed: bool,
}

#[cfg(test)]
impl MemStore {
    /// One mutating statement; models a statement error part-way through a turn.
    fn gate(&mut self) -> StoreResult<()> {
        if let Some(n) = self.fail_in {
            if n <= 1 {
                self.fail_in = None;
                self.failed = true;
                return Err(StoreError("injected storage failure".into()));
            }
            self.fail_in = Some(n - 1);
        }
        Ok(())
    }
}

#[cfg(test)]
impl Store for MemStore {
    fn load_meta(&self) -> StoreResult<Meta> {
        Ok(self.meta.clone().unwrap_or_default())
    }
    fn save_meta(&mut self, meta: &Meta) -> StoreResult<()> {
        self.gate()?;
        self.meta = Some(meta.clone());
        Ok(())
    }
    fn latest_snapshot(&self) -> StoreResult<Option<SnapshotRow>> {
        Ok(self.snapshots.values().next_back().cloned())
    }
    fn snapshot_get(&self, seq: i64) -> StoreResult<Option<SnapshotRow>> {
        Ok(self.snapshots.get(&seq).cloned())
    }
    fn put_snapshot(&mut self, snap: &SnapshotRow, _now_ms: i64) -> StoreResult<()> {
        self.gate()?;
        self.snapshots.insert(snap.seq, snap.clone());
        Ok(())
    }
    fn prune_snapshots(&mut self, latest_seq: i64, keep: [Option<i64>; 2]) -> StoreResult<()> {
        self.gate()?;
        self.snapshots
            .retain(|seq, _| *seq >= latest_seq || keep.contains(&Some(*seq)));
        Ok(())
    }
    fn take_failure(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }
    fn log_max_seq(&self) -> StoreResult<i64> {
        Ok(self.log.keys().next_back().copied().unwrap_or(0))
    }
    fn log_after(&self, after_seq: i64, limit: usize) -> StoreResult<Vec<LogEntry>> {
        Ok(self
            .log
            .range(after_seq + 1..)
            .take(limit)
            .map(|(_, e)| e.clone())
            .collect())
    }
    fn log_get(&self, seq: i64) -> StoreResult<Option<LogEntry>> {
        Ok(self.log.get(&seq).cloned())
    }
    fn touched_after(&self, after_seq: i64) -> StoreResult<Vec<(i64, Vec<String>)>> {
        Ok(self
            .log
            .range(after_seq + 1..)
            .map(|(s, e)| (*s, e.touched.clone()))
            .collect())
    }
    fn append(&mut self, entry: &LogEntry, op: &OpRecord) -> StoreResult<()> {
        let next = self
            .log
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0)
            .max(self.snapshots.keys().next_back().copied().unwrap_or(0))
            + 1;
        if entry.seq != next {
            return Err(StoreError(format!(
                "log seq must be {next}, got {}",
                entry.seq
            )));
        }
        self.gate()?;
        self.log.insert(entry.seq, entry.clone());
        // A failure here leaves the log row without its op id (a partial turn).
        self.gate()?;
        self.ops.insert(op.op_id.clone(), op.clone());
        Ok(())
    }
    fn compact_log(&mut self, through_seq: i64) -> StoreResult<()> {
        self.gate()?;
        self.log.retain(|seq, _| *seq > through_seq);
        Ok(())
    }
    fn op_get(&self, op_id: &str, now_ms: i64) -> StoreResult<Option<OpRecord>> {
        Ok(self
            .ops
            .get(op_id)
            .filter(|o| o.expires_ms > now_ms)
            .cloned())
    }
    fn prune_ops(&mut self, now_ms: i64, keep: i64) -> StoreResult<()> {
        self.gate()?;
        self.ops.retain(|_, o| o.expires_ms > now_ms);
        let mut by_seq: Vec<(i64, String)> = self
            .ops
            .values()
            .map(|o| (o.seq, o.op_id.clone()))
            .collect();
        by_seq.sort_by(|a, b| b.cmp(a));
        for (_, id) in by_seq.into_iter().skip(keep.max(0) as usize) {
            self.ops.remove(&id);
        }
        Ok(())
    }
    fn next_op_expiry(&self) -> StoreResult<Option<i64>> {
        Ok(self.ops.values().map(|o| o.expires_ms).min())
    }
    fn presence_get(&self, client_id: &str) -> StoreResult<Option<PresenceRow>> {
        Ok(self.presence.get(client_id).cloned())
    }
    fn presence_put(&mut self, row: &PresenceRow) -> StoreResult<()> {
        self.presence.insert(row.client_id.clone(), row.clone());
        Ok(())
    }
    fn presence_delete(&mut self, client_id: &str) -> StoreResult<()> {
        self.presence.remove(client_id);
        Ok(())
    }
    fn presence_list(&self, now_ms: i64) -> StoreResult<Vec<PresenceRow>> {
        Ok(self
            .presence
            .values()
            .filter(|p| p.expires_ms > now_ms)
            .cloned()
            .collect())
    }
    fn presence_prune(&mut self, now_ms: i64) -> StoreResult<usize> {
        let before = self.presence.len();
        self.presence.retain(|_, p| p.expires_ms > now_ms);
        Ok(before - self.presence.len())
    }
    fn next_presence_expiry(&self) -> StoreResult<Option<i64>> {
        Ok(self.presence.values().map(|p| p.expires_ms).min())
    }
}
